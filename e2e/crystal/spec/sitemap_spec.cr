require "./spec_helper"

describe Crawlberg do
  describe "sitemap" do
    it "Parses a standard urlset sitemap" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/sitemap_basic"
      __result = Crawlberg.map_urls(engine, url)
      __result.urls.size.should eq(4)
    end
    it "Parses a gzip-compressed sitemap file" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"respect_robots_txt\":false}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/sitemap_compressed_gzip"
      __result = Crawlberg.map_urls(engine, url)
      __result.urls.size.should eq(3)
    end
    it "Handles empty sitemap gracefully" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/sitemap_empty"
      __result = Crawlberg.map_urls(engine, url)
      __result.urls.size.should eq(0)
    end
    it "Discovers sitemap via robots.txt Sitemap directive" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"respect_robots_txt\":true}"))
      url = ENV["MOCK_SERVER_SITEMAP_FROM_ROBOTS_TXT"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/sitemap_from_robots_txt"
      __result = Crawlberg.map_urls(engine, url)
      __result.urls.size.should eq(4)
    end
    it "Follows sitemap index to discover child sitemaps" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{}"))
      url = ENV["MOCK_SERVER_SITEMAP_INDEX"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/sitemap_index"
      __result = Crawlberg.map_urls(engine, url)
      __result.urls.size.should eq(3)
    end
    it "Filters sitemap URLs by lastmod date" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"respect_robots_txt\":false}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/sitemap_lastmod_filter"
      __result = Crawlberg.map_urls(engine, url)
      __result.urls.size.should eq(4)
    end
    it "Uses sitemap URLs exclusively without following page links" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"respect_robots_txt\":false}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/sitemap_only_mode"
      __result = Crawlberg.map_urls(engine, url)
      __result.urls.size.should eq(4)
    end
    it "Parses sitemap with XHTML namespace alternate links" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"respect_robots_txt\":false}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/sitemap_xhtml_links"
      __result = Crawlberg.map_urls(engine, url)
      __result.urls.size.should eq(2)
    end
  end
end
