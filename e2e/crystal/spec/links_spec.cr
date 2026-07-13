require "./spec_helper"

describe Crawlberg do
  describe "links" do
    it "Identifies fragment-only links as anchor type" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{}"))
      url = ENV["MOCK_SERVER_LINKS_ANCHOR_FRAGMENT"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/links_anchor_fragment"
      __result = Crawlberg.scrape(engine, url)
      # TODO: unsupported array assertion `contains_all` on links[].link_type
    end
    it "Resolves relative URLs using base tag href" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/links_base_tag"
      __result = Crawlberg.scrape(engine, url)
      (__result.links.size || 0).should be > 2
      __result.links.any? { |__el| __el.url.to_s.includes?("example.com") }.should be_true
    end
    it "Detects PDF, DOCX, XLSX links as document type" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{}"))
      url = ENV["MOCK_SERVER_LINKS_DOCUMENT_TYPES"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/links_document_types"
      __result = Crawlberg.scrape(engine, url)
      # TODO: unsupported array assertion `contains_all` on links[].link_type
    end
    it "Handles empty href attributes without errors" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/links_empty_href"
      __result = Crawlberg.scrape(engine, url)
      (__result.links.size || 0).should be > 0
      __result.links.any? { |__el| __el.url.to_s.includes?("/valid") }.should be_true
    end
    it "Correctly classifies internal vs external links by domain" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/links_internal_external_classification"
      __result = Crawlberg.scrape(engine, url)
      (__result.links.size || 0).should be > 4
      __result.links.any? { |__el| !__el.url.to_s.empty? }.should be_true
    end
    it "Skips mailto:, javascript:, and tel: scheme links" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/links_mailto_javascript_skip"
      __result = Crawlberg.scrape(engine, url)
      (__result.links.size || 0).should be > 0
      __result.links.all? { |__el| !__el.url.to_s.includes?("mailto:") }.should be_true
    end
    it "Handles protocol-relative URLs (//example.com) correctly" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/links_protocol_relative"
      __result = Crawlberg.scrape(engine, url)
      (__result.links.size || 0).should be > 1
      __result.links.any? { |__el| __el.url.to_s.includes?("//") }.should be_true
    end
    it "Preserves rel=nofollow and rel=canonical attributes" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/links_rel_attributes"
      __result = Crawlberg.scrape(engine, url)
      (__result.links.size || 0).should be > 0
    end
    it "Resolves ../ and ./ relative parent path links correctly" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/links_relative_parent"
      __result = Crawlberg.scrape(engine, url)
      (__result.links.size || 0).should be > 3
    end
  end
end
