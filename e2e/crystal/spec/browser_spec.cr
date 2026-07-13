require "./spec_helper"

describe Crawlberg do
  describe "browser" do
    it "Browser mode 'never' prevents browser use even when JS render hint is set" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"browser\":{\"mode\":\"never\"}}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/browser_config_auto_no_feature"
      __result = Crawlberg.scrape(engine, url)
      __result.status_code.should eq(200)
      __result.js_render_hint.should eq(true)
      __result.browser_used.should eq(false)
    end
    it "Browser mode 'never' prevents browser fallback even for SPA shell content" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"browser\":{\"mode\":\"never\"}}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/browser_config_never_mode"
      __result = Crawlberg.scrape(engine, url)
      __result.status_code.should eq(200)
      __result.js_render_hint.should eq(true)
      __result.browser_used.should eq(false)
    end
    it "Crawl with browser mode 'always' follows links using browser rendering" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"browser\":{\"mode\":\"always\"},\"max_depth\":1,\"respect_robots_txt\":false}"))
      url = ENV["MOCK_SERVER_BROWSER_CRAWL_MODE_ALWAYS"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/browser_crawl_mode_always"
      __result = Crawlberg.crawl(engine, url)
      (__result.pages.size || 0).should be >= 2
      __result.browser_used.should eq(true)
    end
    it "Crawl with browser mode 'auto' falls back to browser when encountering WAF 403" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"browser\":{\"mode\":\"auto\"},\"max_depth\":1,\"respect_robots_txt\":false}"))
      url = ENV["MOCK_SERVER_BROWSER_CRAWL_WAF_FALLBACK"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/browser_crawl_waf_fallback"
      __result = Crawlberg.crawl(engine, url)
      (__result.pages.size || 0).should be >= 1
    end
    it "Does NOT flag a short but real content page as needing JS rendering" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/browser_detect_minimal_page"
      __result = Crawlberg.scrape(engine, url)
      __result.status_code.should eq(200)
      __result.js_render_hint.should eq(false)
      __result.browser_used.should eq(false)
    end
    it "Detects Next.js page with __NEXT_DATA__ but no rendered content as needing JS rendering" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"browser\":{\"mode\":\"never\"}}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/browser_detect_next_empty"
      __result = Crawlberg.scrape(engine, url)
      __result.status_code.should eq(200)
      __result.js_render_hint.should eq(true)
      __result.browser_used.should eq(false)
    end
    it "Does NOT flag Next.js page with full SSR content as needing JS rendering" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/browser_detect_next_rendered"
      __result = Crawlberg.scrape(engine, url)
      __result.status_code.should eq(200)
      __result.html.to_s.should_not be_empty
      __result.js_render_hint.should eq(false)
      __result.browser_used.should eq(false)
    end
    it "Does NOT flag a normal server-rendered page as needing JS rendering" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/browser_detect_normal_page"
      __result = Crawlberg.scrape(engine, url)
      __result.status_code.should eq(200)
      __result.js_render_hint.should eq(false)
      __result.browser_used.should eq(false)
    end
    it "Detects Nuxt SPA shell with empty #__nuxt div as needing JS rendering" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"browser\":{\"mode\":\"never\"}}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/browser_detect_nuxt_shell"
      __result = Crawlberg.scrape(engine, url)
      __result.status_code.should eq(200)
      __result.js_render_hint.should eq(true)
      __result.browser_used.should eq(false)
    end
    it "Detects React SPA shell with empty #root div as needing JS rendering" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"browser\":{\"mode\":\"never\"}}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/browser_detect_react_shell"
      __result = Crawlberg.scrape(engine, url)
      __result.status_code.should eq(200)
      __result.html.to_s.should_not be_empty
      __result.js_render_hint.should eq(true)
      __result.browser_used.should eq(false)
    end
    it "Detects Vue SPA shell with empty #app div as needing JS rendering" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"browser\":{\"mode\":\"never\"}}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/browser_detect_vue_shell"
      __result = Crawlberg.scrape(engine, url)
      __result.status_code.should eq(200)
      __result.js_render_hint.should eq(true)
      __result.browser_used.should eq(false)
    end
    it "Browser extra_wait adds additional time after network_idle to ensure all async operations complete" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"browser\":{\"extra_wait\":200,\"mode\":\"always\",\"wait\":\"network_idle\"}}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/browser_extra_wait"
      __result = Crawlberg.scrape(engine, url)
      __result.browser_used.should eq(true)
    end
    it "Browser auto re-fetches SPA shell when JS rendering is detected" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"browser\":{\"mode\":\"always\"}}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/browser_fallback_spa_render"
      __result = Crawlberg.scrape(engine, url)
      __result.js_render_hint.should eq(true)
      __result.browser_used.should eq(true)
    end
    it "Browser fallback is used when browser mode is always, simulating WAF-blocked scenario" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"browser\":{\"mode\":\"always\"}}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/browser_fallback_waf_blocked"
      __result = Crawlberg.scrape(engine, url)
      __result.browser_used.should eq(true)
    end
    it "Browser mode 'always' uses browser even for normal server-rendered pages" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"browser\":{\"mode\":\"always\"}}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/browser_mode_always"
      __result = Crawlberg.scrape(engine, url)
      __result.browser_used.should eq(true)
    end
    it "Browser profile configuration persists and reuses browser state across crawl sessions" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"browser\":{\"mode\":\"always\"},\"browser_profile\":\"test-profile\"}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/browser_profile_basic"
      __result = Crawlberg.scrape(engine, url)
      __result.browser_used.should eq(true)
      __result.status_code.should eq(200)
    end
    it "Browser wait strategy 'fixed' waits for a specific duration after page navigation" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"browser\":{\"extra_wait\":100,\"mode\":\"always\",\"wait\":\"fixed\"}}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/browser_wait_fixed"
      __result = Crawlberg.scrape(engine, url)
      __result.browser_used.should eq(true)
      __result.status_code.should eq(200)
    end
    it "Browser wait strategy 'selector' waits for specific CSS selector before considering page loaded" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"browser\":{\"mode\":\"always\",\"wait\":\"selector\",\"wait_selector\":\"#content\"}}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/browser_wait_selector"
      __result = Crawlberg.scrape(engine, url)
      __result.browser_used.should eq(true)
      __result.status_code.should eq(200)
    end
  end
end
