require "./spec_helper"

describe Crawlberg do
  describe "auth" do
    it "Sends HTTP Basic authentication header" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"auth\":{\"password\":\"testpass\",\"type\":\"basic\",\"username\":\"testuser\"},\"respect_robots_txt\":false}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/auth_basic_http"
      __result = Crawlberg.scrape(engine, url)
      __result.auth_header_sent.should eq(true)
      __result.status_code.should eq(200)
    end
    it "Sends Bearer token in Authorization header" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"auth\":{\"token\":\"eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.test\",\"type\":\"bearer\"},\"respect_robots_txt\":false}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/auth_bearer_token"
      __result = Crawlberg.scrape(engine, url)
      __result.auth_header_sent.should eq(true)
      __result.status_code.should eq(200)
    end
    it "Sends authentication via custom header (X-API-Key)" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"auth\":{\"name\":\"X-API-Key\",\"type\":\"header\",\"value\":\"sk-test-key-12345\"},\"respect_robots_txt\":false}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/auth_custom_header"
      __result = Crawlberg.scrape(engine, url)
      __result.auth_header_sent.should eq(true)
      __result.status_code.should eq(200)
    end
  end
end
