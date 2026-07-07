// @ts-check
import { defineConfig } from "astro/config";
import starlight from "@astrojs/starlight";
import starlightLlmsTxt from "starlight-llms-txt";
// Local link during migration; switch to "^0.1.0" once @xberg-io/docs-theme is published.
import { xbergStarlightConfig } from "@xberg-io/docs-theme";

const API_LANGUAGES = [
  { label: "Rust", slug: "reference/api-rust" },
  { label: "Python", slug: "reference/api-python" },
  { label: "TypeScript / Node.js", slug: "reference/api-typescript" },
  { label: "Go", slug: "reference/api-go" },
  { label: "Java", slug: "reference/api-java" },
  { label: "Kotlin (Android)", slug: "reference/api-kotlin-android" },
  { label: "C#", slug: "reference/api-csharp" },
  { label: "Ruby", slug: "reference/api-ruby" },
  { label: "PHP", slug: "reference/api-php" },
  { label: "Elixir", slug: "reference/api-elixir" },
  { label: "Dart", slug: "reference/api-dart" },
  { label: "Swift", slug: "reference/api-swift" },
  { label: "Zig", slug: "reference/api-zig" },
  { label: "WebAssembly", slug: "reference/api-wasm" },
  { label: "C / FFI", slug: "reference/api-c" },
];

// https://astro.build/config
export default defineConfig({
  site: "https://docs.crawlberg.xberg.io",
  integrations: [
    starlight(
      xbergStarlightConfig({
        title: "crawlberg",
        description:
          "High-performance Rust web crawling and scraping with always-on HTML→Markdown, " +
          "browser fallback, REST/MCP servers, and native bindings for 14 languages.",
        githubUrl: "https://github.com/xberg-io/crawlberg",
        editBaseUrl: "https://github.com/xberg-io/crawlberg/edit/main/docs-site/",
        plugins: [starlightLlmsTxt()],
        sidebar: [
          { label: "Home", link: "/" },
          {
            label: "Get Started",
            items: [
              { label: "Installation", slug: "getting-started/installation" },
              { label: "Basic Usage", slug: "getting-started/basic_usage" },
              { label: "Quick Start", slug: "getting-started/quickstart" },
            ],
          },
          {
            label: "Guides",
            items: [
              {
                label: "Core",
                items: [
                  { label: "Crawling", slug: "guides/crawling" },
                  { label: "Scraping", slug: "guides/scraping" },
                  { label: "URL Discovery", slug: "guides/url-discovery" },
                  { label: "Markdown Conversion", slug: "guides/markdown" },
                  { label: "Configuration", slug: "guides/configuration" },
                ],
              },
              {
                label: "Advanced",
                items: [
                  { label: "Browser Automation", slug: "guides/browser" },
                  { label: "WARC Output", slug: "guides/warc" },
                  { label: "Antibot Strategy", slug: "antibot" },
                  { label: "Streaming Crawls", slug: "guides/streaming-crawl" },
                ],
              },
              {
                label: "Deployment",
                items: [
                  { label: "Docker", slug: "guides/docker" },
                  { label: "API Server", slug: "guides/api-server" },
                  { label: "MCP Server", slug: "guides/mcp-server" },
                  { label: "Observability", slug: "observability" },
                ],
              },
              { label: "AI Coding Assistants", slug: "guides/ai-coding-assistants" },
              { label: "CLI", slug: "cli/usage" },
            ],
          },
          {
            label: "Concepts",
            items: [
              { label: "Architecture", slug: "concepts/architecture" },
              { label: "Content Extraction", slug: "concepts/content-extraction" },
              { label: "SSRF Defense", slug: "concepts/ssrf-defense" },
            ],
          },
          {
            label: "Reference",
            items: [
              { label: "API", items: API_LANGUAGES },
              { label: "REST API", slug: "reference/rest-api" },
              { label: "MCP Tools", slug: "reference/mcp-tools" },
              { label: "Configuration", slug: "reference/configuration" },
              { label: "Types", slug: "reference/types" },
              { label: "Parsers", slug: "reference/parsers" },
              { label: "Errors", slug: "reference/errors" },
            ],
          },
          {
            label: "More",
            items: [
              { label: "Features", slug: "features" },
              { label: "Contributing", slug: "contributing" },
              { label: "Changelog", slug: "changelog" },
              { label: "Ecosystem", slug: "ecosystem" },
            ],
          },
        ],
      }),
    ),
  ],
});
