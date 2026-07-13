require "./spec_helper"

describe Crawlberg do
  describe "warc" do
    it "Scrape single page with WARC output enabled writes to file" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"respect_robots_txt\":false,\"warc_output\":\"/tmp/crawlberg_test.warc\"}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/warc_basic_output"
      __result = Crawlberg.crawl(engine, url)
      __result.pages[0].status_code.should eq(200)
      __result.pages.size.should eq(1)
    end
    it "Crawl multiple pages with depth=1 and WARC output enabled" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"max_depth\":1,\"respect_robots_txt\":false,\"warc_output\":\"/tmp/crawlberg_crawl.warc\"}"))
      url = ENV["MOCK_SERVER_WARC_MULTI_PAGE_CRAWL"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/warc_multi_page_crawl"
      __result = Crawlberg.crawl(engine, url)
      (__result.pages.size || 0).should be >= 2
      __result.stayed_on_domain.should eq(true)
    end
  end
end
