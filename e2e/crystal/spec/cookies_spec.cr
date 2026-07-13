require "./spec_helper"

describe Crawlberg do
  describe "cookies" do
    it "Isolates cookies per domain during crawl" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"cookies_enabled\":true,\"max_depth\":1,\"respect_robots_txt\":false}"))
      url = ENV["MOCK_SERVER_COOKIES_PER_DOMAIN"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/cookies_per_domain"
      __result = Crawlberg.crawl(engine, url)
      __result.cookies.size.should eq(1)
      __result.cookies.to_s.should contain("domain_cookie")
    end
    it "Maintains cookies across multiple crawl requests" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"cookies_enabled\":true,\"max_depth\":1,\"respect_robots_txt\":false}"))
      url = ENV["MOCK_SERVER_COOKIES_PERSISTENCE"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/cookies_persistence"
      __result = Crawlberg.crawl(engine, url)
      __result.cookies.to_s.should contain("session")
    end
    it "Respects Set-Cookie header from server responses" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"cookies_enabled\":true,\"max_depth\":1,\"respect_robots_txt\":false}"))
      url = ENV["MOCK_SERVER_COOKIES_SET_COOKIE_RESPONSE"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/cookies_set_cookie_response"
      __result = Crawlberg.crawl(engine, url)
      __result.cookies.to_s.should contain("tracking")
    end
  end
end
