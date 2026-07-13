require "./spec_helper"

describe Crawlberg do
  describe "proxy" do
    it "Proxy with username and password credentials authenticates successfully" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"proxy\":{\"password\":\"proxypass\",\"url\":\"http://127.0.0.1:8889\",\"username\":\"proxyuser\"},\"respect_robots_txt\":false}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/proxy_authenticated"
      __result = Crawlberg.crawl(engine, url)
      __result.pages.size.should eq(0)
    end
    it "Configure proxy URL and successfully crawl through it" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"proxy\":{\"url\":\"http://127.0.0.1:8888\"},\"respect_robots_txt\":false}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/proxy_basic_success"
      __result = Crawlberg.crawl(engine, url)
      __result.pages.size.should eq(0)
    end
  end
end
