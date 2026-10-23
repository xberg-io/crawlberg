import assert from "node:assert/strict";
import { createRequire } from "node:module";
import test from "node:test";

const require = createRequire(import.meta.url);

// ~keep Public runtime exports of @xberg-io/crawlberg 1.12.0, including NAPI aliases omitted by its declarations.
const releasedExports = [
  "AssetCategory",
  "BatchCrawlStreamIterator",
  "BrowserBackend",
  "BrowserMode",
  "BrowserWait",
  "ContentFilterKind",
  "CrawlEngineHandle",
  "CrawlStrategyKind",
  "CrawlStreamIterator",
  "DocumentContentEncoding",
  "FeedType",
  "ImageSource",
  "JsAssetCategory",
  "JsBrowserBackend",
  "JsBrowserMode",
  "JsBrowserWait",
  "JsContentFilterKind",
  "JsCrawlEngineHandle",
  "JsCrawlStrategyKind",
  "JsDocumentContentEncoding",
  "JsFeedType",
  "JsImageSource",
  "JsLinkType",
  "JsScrollDirection",
  "LinkType",
  "ScrollDirection",
  "batchCrawl",
  "batchCrawlStream",
  "batchScrape",
  "browserConfigDefault",
  "contentConfigDefault",
  "crawl",
  "crawlConfigDefault",
  "crawlStream",
  "createEngine",
  "generateCitations",
  "interact",
  "mapUrls",
  "scrape",
  "ssrfPolicyDefault",
  "ssrfPolicyFromEnv",
];

test("should_preserve_all_released_runtime_exports_for_commonjs_and_esm_consumers", async (context) => {
  const commonjs = require("@xberg-io/crawlberg");
  const esm = await import("@xberg-io/crawlberg");
  const missingCommonjs = releasedExports.filter((name) => !Object.hasOwn(commonjs, name));
  const missingEsm = releasedExports.filter((name) => !Object.hasOwn(esm, name));
  context.diagnostic(
    `Released Node exports: expected=${releasedExports.length} commonjs=${releasedExports.length - missingCommonjs.length} esm=${releasedExports.length - missingEsm.length}`,
  );
  assert.equal(releasedExports.length, 41);
  assert.deepEqual(missingCommonjs, [], "CommonJS must retain the released runtime API");
  assert.deepEqual(missingEsm, [], "ESM named imports must retain the released runtime API");
  assert.equal(esm.default, commonjs, "Default imports must expose the same native binding as require");
  for (const name of releasedExports) {
    // ~keep Released Js-prefixed names link successfully while their native values are undefined.
    if (!name.startsWith("Js")) {
      assert.notEqual(commonjs[name], undefined, `${name} must expose a native value`);
    }
    assert.equal(esm[name], commonjs[name], `${name} must resolve to the same runtime value in both module formats`);
  }
});
