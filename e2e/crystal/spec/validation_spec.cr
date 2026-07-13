require "./spec_helper"

describe Crawlberg do
  describe "validation" do
    it "Browser endpoint must be a valid ws:// or wss:// URL, not http://" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"browser\":{\"endpoint\":\"http://not-websocket:3000\",\"mode\":\"always\"}}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/validation_browser_endpoint_invalid"
      expect_raises(Exception) do
        Crawlberg.scrape(engine, url)
      end
    end
    it "auth object with empty username in basic auth is rejected" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"auth\":{\"password\":\"secret\",\"type\":\"basic\",\"username\":\"\"}}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/validation_invalid_auth_config"
      expect_raises(Exception) do
        Crawlberg.scrape(engine, url)
      end
    end
    it "Invalid regex in exclude_paths is rejected" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"exclude_paths\":[\"(unclosed\"]}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/validation_invalid_exclude_regex"
      expect_raises(Exception) do
        Crawlberg.scrape(engine, url)
      end
    end
    it "Invalid regex in include_paths is rejected" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"include_paths\":[\"[invalid\"]}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/validation_invalid_include_regex"
      expect_raises(Exception) do
        Crawlberg.scrape(engine, url)
      end
    end
    it "proxy with invalid URL like 'not-a-url' is rejected" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"proxy\":{\"url\":\"not-a-url\"}}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/validation_invalid_proxy_url"
      expect_raises(Exception) do
        Crawlberg.scrape(engine, url)
      end
    end
    it "Retry code outside 100-599 is rejected" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"retry_codes\":[999]}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/validation_invalid_retry_code"
      expect_raises(Exception) do
        Crawlberg.scrape(engine, url)
      end
    end
    it "max_concurrent=0 is rejected as invalid config (minimum is 1)" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"max_concurrent\":0}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/validation_max_concurrent_zero"
      expect_raises(Exception) do
        Crawlberg.scrape(engine, url)
      end
    end
    it "max_depth=200 exceeds limit of 100" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"max_depth\":200}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/validation_max_depth_too_high"
      expect_raises(Exception) do
        Crawlberg.scrape(engine, url)
      end
    end
    it "max_pages=0 is rejected as invalid config" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"max_pages\":0}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/validation_max_pages_zero"
      expect_raises(Exception) do
        Crawlberg.scrape(engine, url)
      end
    end
    it "max_redirects > 100 is rejected as invalid config" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"max_redirects\":200}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/validation_max_redirects_too_high"
      expect_raises(Exception) do
        Crawlberg.scrape(engine, url)
      end
    end
    it "max_body_size set to -1 is rejected as invalid config" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"max_body_size\":0}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/validation_negative_body_size"
      expect_raises(Exception) do
        Crawlberg.scrape(engine, url)
      end
    end
    it "Zero request timeout is rejected as invalid config" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"request_timeout\":0}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/validation_timeout_zero"
      expect_raises(Exception) do
        Crawlberg.scrape(engine, url)
      end
    end
  end
end
