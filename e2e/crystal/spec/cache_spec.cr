require "./spec_helper"

describe Crawlberg do
  describe "cache" do
    it "Crawling with disk cache enabled succeeds without errors" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/cache_basic"
      __result = Crawlberg.scrape(engine, url)
      __result.status_code.should eq(200)
    end
    it "Etag header enables conditional requests for cached content" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"max_depth\":1}"))
      url = ENV["MOCK_SERVER_CACHE_ETAG_CONDITIONAL"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/cache_etag_conditional"
      __result = Crawlberg.crawl(engine, url)
      (__result.pages.size || 0).should be >= 1
      __result.pages[0].status_code.should eq(200)
    end
    it "Last-Modified header enables conditional requests via If-Modified-Since" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"max_depth\":1}"))
      url = ENV["MOCK_SERVER_CACHE_LAST_MODIFIED"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/cache_last_modified"
      __result = Crawlberg.crawl(engine, url)
      (__result.pages.size || 0).should be >= 1
    end
    it "Uncached URLs are fetched fresh without conditional headers" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"max_depth\":1}"))
      url = ENV["MOCK_SERVER_CACHE_MISS_FRESH_FETCH"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/cache_miss_fresh_fetch"
      __result = Crawlberg.crawl(engine, url)
      __result.pages.size.should eq(3)
      __result.pages[0].status_code.should eq(200)
    end
  end
end
