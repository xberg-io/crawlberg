require "./spec_helper"

describe Crawlberg do
  describe "map" do
    it "Discovers all URLs on a site without fetching full content" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"max_depth\":0,\"respect_robots_txt\":false}"))
      url = ENV["MOCK_SERVER_MAP_DISCOVER_URLS"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/map_discover_urls"
      __result = Crawlberg.map_urls(engine, url)
      (__result.urls.size || 0).should be >= 3
    end
    it "Excludes URLs matching patterns from URL map" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"exclude_paths\":[\"/private/.*\",\"/api/.*\"],\"max_depth\":0,\"respect_robots_txt\":false}"))
      url = ENV["MOCK_SERVER_MAP_EXCLUDE_PATTERNS"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/map_exclude_patterns"
      __result = Crawlberg.map_urls(engine, url)
      __result.urls.size.should eq(1)
    end
    it "Includes subdomain URLs in URL map discovery; page has 1 local and 1 subdomain link" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"allow_subdomains\":true,\"max_depth\":0,\"respect_robots_txt\":false}"))
      url = ENV["MOCK_SERVER_MAP_INCLUDE_SUBDOMAINS"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/map_include_subdomains"
      __result = Crawlberg.map_urls(engine, url)
      (__result.urls.size || 0).should be >= 2
    end
    it "Handles large sitemap with 100+ URLs" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"respect_robots_txt\":false}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/map_large_sitemap"
      __result = Crawlberg.map_urls(engine, url)
      (__result.urls.size || 0).should be >= 100
    end
    it "Limits map result count to specified maximum" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"map_limit\":5,\"max_depth\":0,\"respect_robots_txt\":false}"))
      url = ENV["MOCK_SERVER_MAP_LIMIT_PAGINATION"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/map_limit_pagination"
      __result = Crawlberg.map_urls(engine, url)
      (__result.urls.size || 0).should be <= 5
    end
    it "Filters map results by search keyword; 4 links in page but only 2 match 'blog'" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"map_search\":\"blog\",\"max_depth\":0,\"respect_robots_txt\":false}"))
      url = ENV["MOCK_SERVER_MAP_SEARCH_FILTER"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/map_search_filter"
      __result = Crawlberg.map_urls(engine, url)
      (__result.urls.size || 0).should be >= 2
      (__result.urls.size || 0).should be <= 2
    end
  end
end
