require "./spec_helper"

describe Crawlberg do
  describe "concurrent" do
    it "Concurrent crawling fetches all pages with max_concurrent workers" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"max_concurrent\":3,\"max_depth\":1}"))
      url = ENV["MOCK_SERVER_CONCURRENT_BASIC"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/concurrent_basic"
      __result = Crawlberg.crawl(engine, url)
      __result.pages.size.should eq(6)
      (__result.pages.size || 0).should be >= 6
    end
    it "Concurrent depth=2 crawl correctly fans out and deduplicates across levels" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"max_concurrent\":3,\"max_depth\":2}"))
      url = ENV["MOCK_SERVER_CONCURRENT_DEPTH_TWO_FAN_OUT"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/concurrent_depth_two_fan_out"
      __result = Crawlberg.crawl(engine, url)
      __result.pages.size.should eq(4)
    end
    it "Concurrent crawling does not exceed max_pages limit even with high concurrency" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"max_concurrent\":5,\"max_depth\":1,\"max_pages\":3}"))
      url = ENV["MOCK_SERVER_CONCURRENT_MAX_PAGES_EXACT"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/concurrent_max_pages_exact"
      __result = Crawlberg.crawl(engine, url)
      (__result.pages.size || 0).should be <= 3
    end
    it "Concurrent crawl handles partial failures gracefully" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"max_concurrent\":3,\"max_depth\":1}"))
      url = ENV["MOCK_SERVER_CONCURRENT_PARTIAL_ERRORS"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/concurrent_partial_errors"
      __result = Crawlberg.crawl(engine, url)
      (__result.pages.size || 0).should be >= 2
    end
    it "Concurrent crawling respects max_pages limit" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"max_concurrent\":2,\"max_depth\":1,\"max_pages\":3}"))
      url = ENV["MOCK_SERVER_CONCURRENT_RESPECTS_MAX_PAGES"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/concurrent_respects_max_pages"
      __result = Crawlberg.crawl(engine, url)
      (__result.pages.size || 0).should be <= 3
    end
  end
end
