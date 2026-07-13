require "./spec_helper"

describe Crawlberg do
  describe "content" do
    it "Handles 204 No Content response gracefully" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/content_204_no_content"
      __result = Crawlberg.scrape(engine, url)
      __result.status_code.should eq(204)
      __result.html.to_s.should be_empty
    end
    it "Handles ISO-8859-1 encoded page correctly" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"respect_robots_txt\":false}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/content_charset_iso8859"
      __result = Crawlberg.scrape(engine, url)
      __result.detected_charset.to_s.strip.should eq("iso-8859-1")
    end
    it "Handles 200 response with empty body gracefully" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"respect_robots_txt\":false}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/content_empty_body"
      __result = Crawlberg.scrape(engine, url)
      __result.status_code.should eq(200)
    end
    it "Handles response with Accept-Encoding gzip negotiation" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"respect_robots_txt\":false}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/content_gzip_compressed"
      __result = Crawlberg.scrape(engine, url)
      __result.html.to_s.should_not be_empty
      __result.status_code.should eq(200)
    end
    it "Respects max body size limit and truncates or skips oversized pages" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"max_body_size\":1024,\"respect_robots_txt\":false}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/content_large_page_limit"
      __result = Crawlberg.scrape(engine, url)
      (__result.body_size || 0).should be < 1025
    end
    pending "Extracts content with aggressive preprocessing, excluding nav, sidebar, footer"
    it "Detects PDF content by Content-Type header when URL has no .pdf extension" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"respect_robots_txt\":false}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/content_pdf_no_extension"
      __result = Crawlberg.scrape(engine, url)
      __result.is_pdf.should eq(true)
    end
    it "Removes specified HTML elements by CSS selector before processing" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"remove_tags\":[\"nav\",\"aside\",\"footer\"],\"respect_robots_txt\":false}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/content_remove_tags"
      __result = Crawlberg.scrape(engine, url)
      __result.html.to_s.should_not be_empty
    end
    it "Handles UTF-8 content with BOM marker correctly" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"respect_robots_txt\":false}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/content_utf8_bom"
      __result = Crawlberg.scrape(engine, url)
      __result.detected_charset.to_s.strip.should eq("utf-8")
      __result.html.to_s.should_not be_empty
    end
  end
end
