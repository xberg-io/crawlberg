require "./spec_helper"

describe Crawlberg do
  describe "batch" do
    it "Batch crawl of 2 seed URLs with links to discover" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"max_depth\":1,\"respect_robots_txt\":false}"))
      base_url = ENV["MOCK_SERVER_BATCH_CRAWL_BASIC"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/batch_crawl_basic"
      urls = ["/seed1", "/seed2"].map { |p| p.starts_with?("http") ? p : "#{base_url}" + p }
      __result = Crawlberg.batch_crawl(engine, urls)
      __result.completed_count.should eq(2)
      __result.failed_count.should eq(0)
      __result.total_count.should eq(2)
    end
    it "Batch crawl where one seed URL returns 404 error" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"max_depth\":1,\"respect_robots_txt\":false}"))
      base_url = ENV["MOCK_SERVER_BATCH_CRAWL_PARTIAL_FAILURE"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/batch_crawl_partial_failure"
      urls = ["/good_seed", "/bad_seed"].map { |p| p.starts_with?("http") ? p : "#{base_url}" + p }
      __result = Crawlberg.batch_crawl(engine, urls)
      __result.completed_count.should eq(1)
      __result.failed_count.should eq(1)
      __result.total_count.should eq(2)
    end
    it "Batch crawl with max_depth=1 config verifying pages are discovered" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"max_depth\":1,\"respect_robots_txt\":false}"))
      base_url = ENV["MOCK_SERVER_BATCH_CRAWL_WITH_CONFIG"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/batch_crawl_with_config"
      urls = ["/seed1", "/seed2"].map { |p| p.starts_with?("http") ? p : "#{base_url}" + p }
      __result = Crawlberg.batch_crawl(engine, urls)
      __result.completed_count.should eq(2)
      __result.failed_count.should eq(0)
    end
    it "Batch scrape with empty batch_urls array returns error" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{}"))
      base_url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/batch_scrape_empty_urls_error"
      urls = [] of String
      expect_raises(Exception) do
        Crawlberg.batch_scrape(engine, urls)
      end
    end
    it "Batch scrape with aggressive preprocessing configuration" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"content\":{\"preprocessing_preset\":\"aggressive\"}}"))
      base_url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/batch_scrape_with_config"
      urls = ["/article1", "/article2"].map { |p| p.starts_with?("http") ? p : "#{base_url}" + p }
      __result = Crawlberg.batch_scrape(engine, urls)
      __result.completed_count.should eq(2)
      __result.failed_count.should eq(0)
      __result.total_count.should eq(2)
    end
    it "Batch scrape of multiple URLs all succeeding" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{}"))
      base_url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/scrape_batch_basic"
      urls = ["/page1", "/page2", "/page3"].map { |p| p.starts_with?("http") ? p : "#{base_url}" + p }
      __result = Crawlberg.batch_scrape(engine, urls)
      __result.completed_count.should eq(3)
      __result.failed_count.should eq(0)
      __result.total_count.should eq(3)
    end
    it "Batch scrape with one URL failing returns partial results" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{}"))
      base_url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/scrape_batch_partial_failure"
      urls = ["/good1", "/bad", "/good2"].map { |p| p.starts_with?("http") ? p : "#{base_url}" + p }
      __result = Crawlberg.batch_scrape(engine, urls)
      __result.completed_count.should eq(2)
      __result.failed_count.should eq(1)
      __result.total_count.should eq(3)
    end
    it "Batch scrape of 2 URLs completes with 2 results" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{}"))
      base_url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/scrape_batch_progress"
      urls = ["/target", "/other"].map { |p| p.starts_with?("http") ? p : "#{base_url}" + p }
      __result = Crawlberg.batch_scrape(engine, urls)
      __result.total_count.should eq(2)
      __result.completed_count.should eq(2)
    end
  end
end
