require "./spec_helper"

describe Crawlberg do
  describe "stealth" do
    it "User-agent rotation config is accepted and crawl succeeds" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"user_agents\":[\"Mozilla/5.0 (Windows NT 10.0)\",\"Chrome/120.0.0.0\"]}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/stealth_ua_rotation_config"
      __result = Crawlberg.scrape(engine, url)
      __result.status_code.should eq(200)
    end
    it "User-agent rotation cycles through multiple agents across multiple requests" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"max_depth\":1,\"max_pages\":3,\"user_agents\":[\"Mozilla/5.0 (Windows NT 10.0; Win64; x64) TestAgent-1\",\"Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15) TestAgent-2\"]}"))
      url = ENV["MOCK_SERVER_STEALTH_UA_ROTATION_ROUND_ROBIN"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/stealth_ua_rotation_round_robin"
      __result = Crawlberg.crawl(engine, url)
      (__result.pages.size || 0).should be >= 2
    end
    it "Custom user-agent string is applied for single domain crawl" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"max_depth\":0,\"stay_on_domain\":true,\"user_agents\":[\"Mozilla/5.0 TestBot/1.0 (+http://example.com/bot)\"]}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/stealth_ua_rotation_single_domain"
      __result = Crawlberg.crawl(engine, url)
      __result.pages[0].status_code.should eq(200)
      __result.pages.size.should eq(1)
    end
  end
end
