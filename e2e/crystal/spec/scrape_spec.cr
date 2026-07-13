require "./spec_helper"

describe Crawlberg do
  describe "scrape" do
    it "Same asset linked twice results in one download with one unique hash" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"download_assets\":true}"))
      url = ENV["MOCK_SERVER_SCRAPE_ASSET_DEDUP"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/scrape_asset_dedup"
      __result = Crawlberg.scrape(engine, url)
      __result.status_code.should eq(200)
      __result.assets.size.should eq(2)
      __result.assets.any? { |__el| !__el.content_hash.to_s.empty? }.should be_true
    end
    it "Skips assets exceeding max_asset_size limit" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"download_assets\":true,\"max_asset_size\":150}"))
      url = ENV["MOCK_SERVER_SCRAPE_ASSET_MAX_SIZE"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/scrape_asset_max_size"
      __result = Crawlberg.scrape(engine, url)
      __result.status_code.should eq(200)
      __result.assets.size.should eq(2)
    end
    it "Only downloads image assets when asset_types filter is set" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"asset_types\":[\"image\"],\"download_assets\":true}"))
      url = ENV["MOCK_SERVER_SCRAPE_ASSET_TYPE_FILTER"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/scrape_asset_type_filter"
      __result = Crawlberg.scrape(engine, url)
      __result.status_code.should eq(200)
      __result.assets.size.should eq(1)
      __result.assets.any? { |__el| __el.asset_category.to_s.includes?("image") }.should be_true
    end
    it "Scrapes a simple HTML page and extracts title, description, and links" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"max_depth\":0,\"respect_robots_txt\":false}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/scrape_basic_html_page"
      __result = Crawlberg.scrape(engine, url)
      __result.status_code.should eq(200)
      __result.content_type.to_s.strip.should eq("text/html")
      __result.html.to_s.should_not be_empty
      __result.try(&.metadata).try(&.title).to_s.strip.should eq("Example Domain")
      __result.try(&.metadata).try(&.description).to_s.should contain("illustrative examples")
      __result.try(&.metadata).try(&.canonical_url).to_s.should_not be_empty
      (__result.links.size || 0).should be > 0
      # TODO: unsupported array assertion `contains_all` on links[].link_type
      __result.images.size.should eq(0)
      __result.try(&.metadata).try(&.og_title).to_s.should be_empty
    end
    it "Classifies links by type: internal, external, anchor, document, image" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/scrape_complex_links"
      __result = Crawlberg.scrape(engine, url)
      __result.status_code.should eq(200)
      (__result.links.size || 0).should be > 9
      __result.links.any? { |__el| !__el.url.to_s.empty? }.should be_true
    end
    it "Downloads CSS, JS, and image assets from page" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{\"download_assets\":true}"))
      url = ENV["MOCK_SERVER_SCRAPE_DOWNLOAD_ASSETS"]? || (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/scrape_download_assets"
      __result = Crawlberg.scrape(engine, url)
      __result.status_code.should eq(200)
      (__result.assets.size || 0).should be > 2
    end
    it "Extracts Dublin Core metadata from a page" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/scrape_dublin_core"
      __result = Crawlberg.scrape(engine, url)
      __result.status_code.should eq(200)
      __result.try(&.metadata).try(&.dc_title).to_s.should_not be_empty
      __result.try(&.metadata).try(&.dc_title).to_s.strip.should eq("Effects of Climate Change on Marine Biodiversity")
      __result.try(&.metadata).try(&.dc_creator).to_s.strip.should eq("Dr. Jane Smith")
    end
    it "Handles an empty HTML document without errors" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/scrape_empty_page"
      __result = Crawlberg.scrape(engine, url)
      __result.status_code.should eq(200)
      (__result.links.size || 0).should be > -1
      __result.images.size.should eq(0)
    end
    it "Discovers RSS, Atom, and JSON feed links" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/scrape_feed_discovery"
      __result = Crawlberg.scrape(engine, url)
      __result.status_code.should eq(200)
      (__result.feeds.size || 0).should be >= 3
    end
    it "Extracts images from img, picture, og:image, twitter:image" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/scrape_image_sources"
      __result = Crawlberg.scrape(engine, url)
      __result.status_code.should eq(200)
      (__result.images.size || 0).should be > 4
      __result.try(&.metadata).try(&.og_image).to_s.strip.should eq("https://example.com/images/og-hero.jpg")
    end
    it "Handles SPA page with JavaScript-only content (no server-rendered HTML)" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/scrape_js_heavy_spa"
      __result = Crawlberg.scrape(engine, url)
      __result.html.to_s.should_not be_empty
    end
    it "Extracts JSON-LD structured data from a page" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/scrape_json_ld"
      __result = Crawlberg.scrape(engine, url)
      __result.status_code.should eq(200)
      __result.json_ld.to_s.should_not be_empty
      __result.json_ld.any? { |__el| __el.schema_type == "Recipe" }.should be_true
      __result.json_ld.any? { |__el| __el.name == "Best Chocolate Cake" }.should be_true
    end
    it "Gracefully handles broken HTML without crashing" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/scrape_malformed_html"
      __result = Crawlberg.scrape(engine, url)
      __result.status_code.should eq(200)
      __result.html.to_s.should_not be_empty
      __result.try(&.metadata).try(&.description).to_s.should contain("broken HTML")
    end
    it "Extracts full Open Graph metadata from a page" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/scrape_og_metadata"
      __result = Crawlberg.scrape(engine, url)
      __result.status_code.should eq(200)
      __result.try(&.metadata).try(&.og_title).to_s.should_not be_empty
      __result.try(&.metadata).try(&.og_title).to_s.strip.should eq("Article Title")
      __result.try(&.metadata).try(&.og_type).to_s.strip.should eq("article")
      __result.try(&.metadata).try(&.og_image).to_s.strip.should eq("https://example.com/images/article-hero.jpg")
      __result.try(&.metadata).try(&.og_description).to_s.should_not be_empty
      __result.try(&.metadata).try(&.title).to_s.strip.should eq("Article Title - Example Blog")
    end
    it "Extracts Twitter Card metadata from a page" do
      engine = Crawlberg.create_engine(Crawlberg::CrawlConfig.from_json("{}"))
      url = (ENV["MOCK_SERVER_URL"]? || "") + "/fixtures/scrape_twitter_card"
      __result = Crawlberg.scrape(engine, url)
      __result.status_code.should eq(200)
      __result.try(&.metadata).try(&.twitter_card).to_s.should_not be_empty
      __result.try(&.metadata).try(&.twitter_card).to_s.strip.should eq("summary_large_image")
      __result.try(&.metadata).try(&.twitter_title).to_s.strip.should eq("New Product Launch")
    end
  end
end
