require "./spec_helper"

describe Crawlberg do
  describe "redirect" do
    it "Follows 301 permanent redirect and returns final page content" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"respect_robots_txt\":false}"))
      url = ENV["MOCK_SERVER_REDIRECT_301_PERMANENT"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/redirect_301_permanent"
      __result = Crawlberg.crawl(engine, url)
      __result.final_url.to_s.should contain("/target")
      __result.redirect_count.should eq(1)
    end
    it "Follows 302 Found redirect correctly" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"respect_robots_txt\":false}"))
      url = ENV["MOCK_SERVER_REDIRECT_302_FOUND"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/redirect_302_found"
      __result = Crawlberg.crawl(engine, url)
      __result.final_url.to_s.should contain("/found-target")
      __result.redirect_count.should eq(1)
    end
    it "Follows 303 See Other redirect (method changes to GET)" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"respect_robots_txt\":false}"))
      url = ENV["MOCK_SERVER_REDIRECT_303_SEE_OTHER"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/redirect_303_see_other"
      __result = Crawlberg.crawl(engine, url)
      __result.final_url.to_s.should contain("/see-other")
      __result.redirect_count.should eq(1)
    end
    it "Follows 307 Temporary Redirect (preserves method)" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"respect_robots_txt\":false}"))
      url = ENV["MOCK_SERVER_REDIRECT_307_TEMPORARY"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/redirect_307_temporary"
      __result = Crawlberg.crawl(engine, url)
      __result.final_url.to_s.should contain("/temp-target")
      __result.redirect_count.should eq(1)
    end
    it "Follows 308 Permanent Redirect (preserves method)" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"respect_robots_txt\":false}"))
      url = ENV["MOCK_SERVER_REDIRECT_308_PERMANENT"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/redirect_308_permanent"
      __result = Crawlberg.crawl(engine, url)
      __result.final_url.to_s.should contain("/perm-target")
      __result.redirect_count.should eq(1)
    end
    it "Follows a chain of redirects (301 -> 302 -> 200)" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"respect_robots_txt\":false}"))
      url = ENV["MOCK_SERVER_REDIRECT_CHAIN"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/redirect_chain"
      __result = Crawlberg.crawl(engine, url)
      __result.final_url.to_s.should contain("/step2")
      __result.redirect_count.should eq(2)
    end
    it "Reports cross-domain redirect target without following to external domain" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"respect_robots_txt\":false}"))
      url = ENV["MOCK_SERVER_REDIRECT_CROSS_DOMAIN"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/redirect_cross_domain"
      __result = Crawlberg.crawl(engine, url)
      __result.final_url.to_s.should contain("/external-redirect")
      __result.redirect_count.should eq(1)
    end
    it "Detects redirect loop (A -> B -> A) and returns error" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"respect_robots_txt\":false}"))
      url = ENV["MOCK_SERVER_REDIRECT_LOOP"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/redirect_loop"
      __result = Crawlberg.crawl(engine, url)
      __result.error.should_not be_nil
    end
    it "Aborts when redirect count exceeds max_redirects limit" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"max_redirects\":2,\"respect_robots_txt\":false}"))
      url = ENV["MOCK_SERVER_REDIRECT_MAX_EXCEEDED"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/redirect_max_exceeded"
      __result = Crawlberg.crawl(engine, url)
      __result.error.should_not be_nil
    end
    it "Follows HTML meta-refresh redirect to target page" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"respect_robots_txt\":false}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/redirect_meta_refresh"
      __result = Crawlberg.crawl(engine, url)
      __result.final_url.to_s.should contain("/target")
      __result.redirect_count.should eq(1)
    end
    it "Handles HTTP Refresh header redirect" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"respect_robots_txt\":false}"))
      url = ENV["MOCK_SERVER_REDIRECT_REFRESH_HEADER"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/redirect_refresh_header"
      __result = Crawlberg.crawl(engine, url)
      __result.final_url.to_s.should contain("/refreshed")
      __result.redirect_count.should eq(1)
    end
    it "Redirect target returns 404 Not Found" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"respect_robots_txt\":false}"))
      url = ENV["MOCK_SERVER_REDIRECT_TO_404"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/redirect_to_404"
      __result = Crawlberg.crawl(engine, url)
      __result.final_url.to_s.should contain("/gone")
      __result.redirect_count.should eq(1)
      __result.error.should_not be_nil
    end
  end
end
