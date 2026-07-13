require "./spec_helper"

describe Crawlberg do
  describe "robots" do
    it "Permissive robots.txt allows all paths" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"respect_robots_txt\":true}"))
      url = ENV["MOCK_SERVER_ROBOTS_ALLOW_ALL"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/robots_allow_all"
      __result = Crawlberg.scrape(engine, url)
      __result.is_allowed.should eq(true)
    end
    it "Allow directive overrides Disallow for specific paths" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"respect_robots_txt\":true}"))
      url = ENV["MOCK_SERVER_ROBOTS_ALLOW_OVERRIDE"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/robots_allow_override"
      __result = Crawlberg.scrape(engine, url)
      __result.is_allowed.should eq(true)
    end
    it "Correctly parses robots.txt with inline and line comments" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"respect_robots_txt\":true,\"user_agent\":\"crawlberg\"}"))
      url = ENV["MOCK_SERVER_ROBOTS_COMMENTS_HANDLING"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/robots_comments_handling"
      __result = Crawlberg.scrape(engine, url)
      __result.is_allowed.should eq(true)
    end
    it "Respects crawl-delay directive from robots.txt" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"respect_robots_txt\":true,\"user_agent\":\"crawlberg\"}"))
      url = ENV["MOCK_SERVER_ROBOTS_CRAWL_DELAY"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/robots_crawl_delay"
      __result = Crawlberg.scrape(engine, url)
      __result.crawl_delay.should eq(2)
    end
    it "Robots.txt disallows specific paths" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"respect_robots_txt\":true}"))
      url = ENV["MOCK_SERVER_ROBOTS_DISALLOW_PATH"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/robots_disallow_path"
      __result = Crawlberg.scrape(engine, url)
      __result.is_allowed.should eq(false)
    end
    it "Detects nofollow meta robots tag and skips link extraction" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"respect_robots_txt\":true}"))
      url = ENV["MOCK_SERVER_ROBOTS_META_NOFOLLOW"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/robots_meta_nofollow"
      __result = Crawlberg.scrape(engine, url)
      __result.nofollow_detected.should eq(true)
    end
    it "Detects noindex meta robots tag in HTML page" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"respect_robots_txt\":true}"))
      url = ENV["MOCK_SERVER_ROBOTS_META_NOINDEX"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/robots_meta_noindex"
      __result = Crawlberg.scrape(engine, url)
      __result.noindex_detected.should eq(true)
    end
    it "Missing robots.txt (404) allows all crawling" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"respect_robots_txt\":true}"))
      url = ENV["MOCK_SERVER_ROBOTS_MISSING_404"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/robots_missing_404"
      __result = Crawlberg.scrape(engine, url)
      __result.is_allowed.should eq(true)
    end
    it "Picks the most specific user-agent block from robots.txt" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"respect_robots_txt\":true,\"user_agent\":\"SpecificBot\"}"))
      url = ENV["MOCK_SERVER_ROBOTS_MULTIPLE_USER_AGENTS"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/robots_multiple_user_agents"
      __result = Crawlberg.scrape(engine, url)
      __result.is_allowed.should eq(true)
    end
    it "Parses request-rate directive from robots.txt" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"respect_robots_txt\":true,\"user_agent\":\"crawlberg\"}"))
      url = ENV["MOCK_SERVER_ROBOTS_REQUEST_RATE"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/robots_request_rate"
      __result = Crawlberg.scrape(engine, url)
      __result.crawl_delay.should eq(5)
      __result.is_allowed.should eq(true)
    end
    it "Discovers sitemap URL from Sitemap directive in robots.txt" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"respect_robots_txt\":true}"))
      url = ENV["MOCK_SERVER_ROBOTS_SITEMAP_DIRECTIVE"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/robots_sitemap_directive"
      __result = Crawlberg.scrape(engine, url)
      __result.is_allowed.should eq(true)
    end
    it "Matches user-agent specific rules in robots.txt" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"respect_robots_txt\":true,\"user_agent\":\"CrawlbergBot\"}"))
      url = ENV["MOCK_SERVER_ROBOTS_USER_AGENT_SPECIFIC"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/robots_user_agent_specific"
      __result = Crawlberg.scrape(engine, url)
      __result.is_allowed.should eq(false)
    end
    it "Handles wildcard Disallow patterns in robots.txt" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"respect_robots_txt\":true}"))
      url = ENV["MOCK_SERVER_ROBOTS_WILDCARD_PATHS"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/robots_wildcard_paths"
      __result = Crawlberg.scrape(engine, url)
      __result.is_allowed.should eq(false)
    end
    it "Respects X-Robots-Tag HTTP header directives" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"respect_robots_txt\":true}"))
      url = ENV["MOCK_SERVER_ROBOTS_X_ROBOTS_TAG"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/robots_x_robots_tag"
      __result = Crawlberg.scrape(engine, url)
      __result.x_robots_tag.to_s.strip.should eq("noindex, nofollow")
      __result.noindex_detected.should eq(true)
      __result.nofollow_detected.should eq(true)
    end
  end
end
