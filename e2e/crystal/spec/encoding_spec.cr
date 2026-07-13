require "./spec_helper"

describe Crawlberg do
  describe "encoding" do
    it "Handles double-encoded URL characters (%25C3%25B6)" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{}"))
      url = ENV["MOCK_SERVER_ENCODING_DOUBLE_ENCODED"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/encoding_double_encoded"
      __result = Crawlberg.scrape(engine, url)
      __result.html.to_s.should_not be_empty
      (__result.links.size || 0).should be >= 1
    end
    it "Handles charset mismatch between HTTP header and HTML meta tag" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/encoding_mixed_charset_page"
      __result = Crawlberg.scrape(engine, url)
      __result.html.to_s.should_not be_empty
    end
    it "Handles percent-encoded spaces and characters in URL paths" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{}"))
      url = ENV["MOCK_SERVER_ENCODING_PERCENT_ENCODED_PATH"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/encoding_percent_encoded_path"
      __result = Crawlberg.scrape(engine, url)
      __result.html.to_s.should_not be_empty
      (__result.links.size || 0).should be >= 2
    end
    it "Handles Unicode characters in URLs (Hebrew, Japanese, Cyrillic)" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/encoding_unicode_url"
      __result = Crawlberg.scrape(engine, url)
      __result.html.to_s.should_not be_empty
    end
  end
end
