require "./spec_helper"

describe Crawlberg do
  describe "crawl" do
    it "Skips image and video content types gracefully" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"max_depth\":1,\"respect_robots_txt\":false}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/content_binary_skip"
      __result = Crawlberg.scrape(engine, url)
      __result.was_skipped.should eq(true)
    end
    it "Encounters PDF link and skips or marks as document type" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"max_depth\":1,\"respect_robots_txt\":false}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/content_pdf_link_skip"
      __result = Crawlberg.scrape(engine, url)
      __result.was_skipped.should eq(true)
    end
    it "Concurrent crawl respects max_depth limit" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"max_concurrent\":3,\"max_depth\":1,\"respect_robots_txt\":false}"))
      url = ENV["MOCK_SERVER_CRAWL_CONCURRENT_DEPTH"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/crawl_concurrent_depth"
      __result = Crawlberg.crawl(engine, url)
      __result.pages.size.should eq(3)
      __result.stayed_on_domain.should eq(true)
    end
    it "Respects max concurrent requests limit during crawl" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"max_concurrent\":2,\"max_depth\":1,\"respect_robots_txt\":false}"))
      url = ENV["MOCK_SERVER_CRAWL_CONCURRENT_LIMIT"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/crawl_concurrent_limit"
      __result = Crawlberg.crawl(engine, url)
      __result.pages.size.should eq(5)
    end
    it "Concurrent crawl respects max_pages budget" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"max_concurrent\":4,\"max_depth\":1,\"max_pages\":3,\"respect_robots_txt\":false}"))
      url = ENV["MOCK_SERVER_CRAWL_CONCURRENT_MAX_PAGES"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/crawl_concurrent_max_pages"
      __result = Crawlberg.crawl(engine, url)
      (__result.pages.size || 0).should be <= 3
    end
    it "Sends custom headers on all crawl requests" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"custom_headers\":{\"Accept-Language\":\"en-US\",\"X-Custom-Header\":\"test-value\"},\"max_depth\":1,\"respect_robots_txt\":false}"))
      url = ENV["MOCK_SERVER_CRAWL_CUSTOM_HEADERS"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/crawl_custom_headers"
      __result = Crawlberg.crawl(engine, url)
      __result.pages.size.should eq(2)
    end
    it "Follows links one level deep from start page" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"max_depth\":1,\"respect_robots_txt\":false}"))
      url = ENV["MOCK_SERVER_CRAWL_DEPTH_ONE"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/crawl_depth_one"
      __result = Crawlberg.crawl(engine, url)
      __result.pages.size.should eq(3)
      __result.stayed_on_domain.should eq(true)
    end
    it "Crawls in breadth-first order, processing depth-0 pages before depth-1" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"max_depth\":2,\"respect_robots_txt\":false}"))
      url = ENV["MOCK_SERVER_CRAWL_DEPTH_PRIORITY"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/crawl_depth_priority"
      __result = Crawlberg.crawl(engine, url)
      __result.pages.size.should eq(4)
    end
    it "Crawls 3 levels deep (depth 0, 1, 2)" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"max_depth\":2,\"respect_robots_txt\":false}"))
      url = ENV["MOCK_SERVER_CRAWL_DEPTH_TWO"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/crawl_depth_two"
      __result = Crawlberg.crawl(engine, url)
      __result.pages.size.should eq(3)
      (__result.pages.size || 0).should be >= 3
    end
    it "Depth=2 crawl follows a chain of links across three levels" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"max_concurrent\":1,\"max_depth\":2}"))
      url = ENV["MOCK_SERVER_CRAWL_DEPTH_TWO_CHAIN"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/crawl_depth_two_chain"
      __result = Crawlberg.crawl(engine, url)
      __result.pages.size.should eq(3)
    end
    it "Normalizes double slashes in URL paths (//page to /page)" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"max_depth\":1,\"respect_robots_txt\":false}"))
      url = ENV["MOCK_SERVER_CRAWL_DOUBLE_SLASH_NORMALIZATION"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/crawl_double_slash_normalization"
      __result = Crawlberg.crawl(engine, url)
      __result.pages.size.should eq(2)
    end
    it "Crawl completes when child page has no outgoing links" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"max_concurrent\":1,\"max_depth\":2}"))
      url = ENV["MOCK_SERVER_CRAWL_EMPTY_PAGE_NO_LINKS"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/crawl_empty_page_no_links"
      __result = Crawlberg.crawl(engine, url)
      __result.pages.size.should eq(2)
    end
    it "Skips URLs matching the exclude path pattern" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"exclude_paths\":[\"/admin/.*\"],\"max_depth\":1,\"respect_robots_txt\":false}"))
      url = ENV["MOCK_SERVER_CRAWL_EXCLUDE_PATH_PATTERN"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/crawl_exclude_path_pattern"
      __result = Crawlberg.crawl(engine, url)
      __result.pages.size.should eq(2)
    end
    it "External links are discovered but not followed when stay_on_domain is true" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"max_concurrent\":1,\"max_depth\":1,\"stay_on_domain\":true}"))
      url = ENV["MOCK_SERVER_CRAWL_EXTERNAL_LINKS_IGNORED"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/crawl_external_links_ignored"
      __result = Crawlberg.crawl(engine, url)
      __result.pages.size.should eq(2)
      __result.stayed_on_domain.should eq(true)
    end
    it "Strips #fragment from URLs for deduplication" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"max_depth\":1,\"respect_robots_txt\":false}"))
      url = ENV["MOCK_SERVER_CRAWL_FRAGMENT_STRIPPING"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/crawl_fragment_stripping"
      __result = Crawlberg.crawl(engine, url)
      __result.pages.size.should eq(2)
    end
    it "Only follows URLs matching the include path pattern" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"include_paths\":[\"/blog/.*\"],\"max_depth\":1,\"respect_robots_txt\":false}"))
      url = ENV["MOCK_SERVER_CRAWL_INCLUDE_PATH_PATTERN"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/crawl_include_path_pattern"
      __result = Crawlberg.crawl(engine, url)
      __result.pages.size.should eq(2)
    end
    it "max_depth=0 crawls only the seed page with no link following" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"max_depth\":0}"))
      url = ENV["MOCK_SERVER_CRAWL_MAX_DEPTH_ZERO"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/crawl_max_depth_zero"
      __result = Crawlberg.crawl(engine, url)
      __result.pages.size.should eq(1)
      (__result.pages.size || 0).should be <= 1
    end
    it "Stops crawling at page budget limit" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"max_pages\":3,\"respect_robots_txt\":false}"))
      url = ENV["MOCK_SERVER_CRAWL_MAX_PAGES"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/crawl_max_pages"
      __result = Crawlberg.crawl(engine, url)
      (__result.pages.size || 0).should be <= 3
    end
    it "Crawl handles links to non-HTML content types gracefully" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"max_concurrent\":1,\"max_depth\":1}"))
      url = ENV["MOCK_SERVER_CRAWL_MIXED_CONTENT_TYPES"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/crawl_mixed_content_types"
      __result = Crawlberg.crawl(engine, url)
      (__result.pages.size || 0).should be >= 2
    end
    it "Multiple linked pages with redirects are handled during crawl traversal" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"max_concurrent\":1,\"max_depth\":1}"))
      url = ENV["MOCK_SERVER_CRAWL_MULTIPLE_REDIRECTS_IN_TRAVERSAL"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/crawl_multiple_redirects_in_traversal"
      __result = Crawlberg.crawl(engine, url)
      (__result.pages.size || 0).should be >= 1
    end
    it "Deduplicates URLs with same query params in different order" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"max_depth\":1,\"respect_robots_txt\":false}"))
      url = ENV["MOCK_SERVER_CRAWL_QUERY_PARAM_DEDUP"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/crawl_query_param_dedup"
      __result = Crawlberg.crawl(engine, url)
      __result.pages.size.should eq(2)
    end
    it "Links that redirect are followed during crawl traversal" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"max_concurrent\":1,\"max_depth\":1}"))
      url = ENV["MOCK_SERVER_CRAWL_REDIRECT_IN_TRAVERSAL"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/crawl_redirect_in_traversal"
      __result = Crawlberg.crawl(engine, url)
      (__result.pages.size || 0).should be >= 1
    end
    it "Page linking to itself does not cause infinite crawl loop" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"max_concurrent\":1,\"max_depth\":1}"))
      url = ENV["MOCK_SERVER_CRAWL_SELF_LINK_NO_LOOP"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/crawl_self_link_no_loop"
      __result = Crawlberg.crawl(engine, url)
      __result.pages.size.should eq(2)
    end
    it "Crawling a page with no links returns only the seed page" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"max_depth\":2}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/crawl_single_page_no_links"
      __result = Crawlberg.crawl(engine, url)
      __result.pages.size.should eq(1)
    end
    it "Does not follow external links when stay_on_domain is true" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"max_depth\":1,\"respect_robots_txt\":false,\"stay_on_domain\":true}"))
      url = ENV["MOCK_SERVER_CRAWL_STAY_ON_DOMAIN"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/crawl_stay_on_domain"
      __result = Crawlberg.crawl(engine, url)
      __result.pages.size.should eq(2)
      __result.stayed_on_domain.should eq(true)
    end
    it "Stays on exact domain and skips subdomain links" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"allow_subdomains\":false,\"max_depth\":1,\"respect_robots_txt\":false,\"stay_on_domain\":true}"))
      url = ENV["MOCK_SERVER_CRAWL_SUBDOMAIN_EXCLUSION"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/crawl_subdomain_exclusion"
      __result = Crawlberg.crawl(engine, url)
      __result.pages.size.should eq(2)
      __result.stayed_on_domain.should eq(true)
    end
    it "Crawls subdomains when allow_subdomains is enabled" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"allow_subdomains\":true,\"max_depth\":1,\"respect_robots_txt\":false}"))
      url = ENV["MOCK_SERVER_CRAWL_SUBDOMAIN_INCLUSION"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/crawl_subdomain_inclusion"
      __result = Crawlberg.crawl(engine, url)
      (__result.pages.size || 0).should be >= 2
    end
    it "Deduplicates /page and /page/ as the same URL" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"max_depth\":1,\"respect_robots_txt\":false}"))
      url = ENV["MOCK_SERVER_CRAWL_TRAILING_SLASH_DEDUP"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/crawl_trailing_slash_dedup"
      __result = Crawlberg.crawl(engine, url)
      __result.pages.size.should eq(2)
    end
    it "Deduplicates URLs that differ only by fragment or query params" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"max_depth\":1,\"respect_robots_txt\":false}"))
      url = ENV["MOCK_SERVER_CRAWL_URL_DEDUPLICATION"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/crawl_url_deduplication"
      __result = Crawlberg.crawl(engine, url)
      (__result.pages.size || 0).should be <= 2
    end
  end
end
