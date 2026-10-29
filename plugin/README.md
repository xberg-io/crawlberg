# crawlberg

Crawl, scrape, and convert websites to Markdown using the local `crawlberg` CLI in your agent.

<!-- ~keep TODO: add screenshot -->

## Install

### From the marketplace (recommended)

Pending review for official Claude marketplace.

Self-host:

```text
/plugin marketplace add xberg-io/crawlberg
/plugin install crawlberg@crawlberg
```

### Kimi installation and compatibility

From a checkout of this repository, install with the Python Kimi CLI version used for native
plugin validation:

```sh
uvx --from kimi-cli==1.51.0 kimi plugin install ./plugin
```

Native plugin installation was validated with Python Kimi CLI 1.51.0. The replacement
`@moonshot-ai/kimi-code` 2.1.1 does not provide a `plugin` CLI subcommand; its interactive
plugin loading has not been verified. These results do not establish compatibility with
newer Kimi releases.

### Pi

From a checkout of this repository:

```bash
pi install ./plugin
```

After the next plugin release, install the same package used by OpenCode:

```bash
pi install npm:@xberg-io/opencode-crawlberg
```

Pi loads the bundled skills and prompt templates. Use `/crawlberg-scrape` or `/crawlberg-crawl` with the input as arguments. Install the `crawlberg` CLI separately for shell access. This package does not register MCP servers, hooks, or native Pi extensions.

### Binary requirement

The MCP server runs through an auto-installing launcher (`scripts/mcp-launch.sh`): on first use it reuses any working `crawlberg` binary already on `PATH`, then tries `npx`, Homebrew, or a prebuilt download. No manual install is required for MCP.

To install the CLI yourself (recommended for direct CLI use, and reused by the launcher):

```bash
brew install xberg-io/tap/crawlberg
# or run it without a persistent install (the CLI proxy package self-installs the binary):
pnpm dlx @xberg-io/crawlberg-cli --help
# or build from source (the mcp/api subcommands are non-default features):
cargo install crawlberg-cli --features all
```

The published npm (`@xberg-io/crawlberg`) and PyPI (`crawlberg`) packages are language *library* bindings, not the CLI — install via Homebrew or the from-source build above for the `crawlberg` binary.

Headless fallback requires Chrome/Chromium on your system. The CLI launches it on demand; skip the binary if you only plan to use `--browser-mode never`.

## Skills shipped

| Skill | Trigger |
|-------|---------|
| **crawlberg** | Crawl, scrape, and convert websites to Markdown using the local crawlberg CLI and its MCP server. Use when the user wants to fetch a page, follow links across a domain, enumerate URLs, or drive a real browser. Covers installation, the subcommands (scrape, crawl, map, interact, batch-scrape, batch-crawl, download, citations, version, mcp, serve), output formats (JSON + Markdown), browser fallback, and when to prefer the MCP server over shelling out. |
| **crawlberg-crawling-a-site** | Use when the user wants to follow links across a domain and capture every reachable page as Markdown. Covers `crawlberg crawl` with depth, page caps, concurrency, rate limiting, domain scoping, robots, and output selection. |
| **crawlberg-scraping-html-to-markdown** | Use when the user wants a single page rendered as clean Markdown plus structured metadata. Covers `crawlberg scrape <url>`, JSON vs Markdown output, what metadata is returned, and how to handle JS-heavy pages. |
| **crawlberg-mapping-urls** | Use when the user wants the list of URLs on a site rather than the page content — sitemap analysis, link planning, or seeding another tool. Covers `crawlberg map <url>` with `--limit`, `--search`, robots, output, and how it differs from a full crawl. |
| **crawlberg-automating-the-browser** | Use when extracting a page needs scripted interaction first — click, type, press a key, scroll, wait, screenshot, or run JS before capturing the DOM. Covers `crawlberg interact <url> --actions` with the real action schema, result shape, limits, and external-CDP options. |
| **crawlberg-serving-the-api** | Use when the user wants a long-running HTTP service for scrape/crawl/map instead of one-shot CLI calls or the MCP server. Covers `crawlberg serve`, the Firecrawl-v1-compatible endpoints, `--host`/`--port`, and when to prefer it. |
| **crawlberg-headless-fallback** | Use when a static fetch returns nothing useful and the page needs a real browser. Covers `--browser-mode auto\|always\|never`, external CDP via `--browser-endpoint`, symptoms of JS-only pages and WAF blocks, and the performance cost. |

## MCP / CLI

MCP-capable integrations wire up the `crawlberg` MCP server via `scripts/mcp-launch.sh`, which resolves an existing binary or installs the latest available CLI, then runs `crawlberg mcp` over stdio. Override binary resolution with `CRAWLBERG_LAUNCHER=auto|download`.

The MCP server exposes nine tools:

- `scrape` — fetch and convert a single URL to Markdown or JSON.
- `crawl` — follow links across a domain, bounded by depth and page count.
- `map` — enumerate URLs from sitemaps and link extraction.
- `batch_scrape` — scrape multiple URLs concurrently.
- `batch_crawl` — crawl multiple seed URLs concurrently.
- `download` — download a document from a URL and report its file metadata.
- `interact` — drive a headless browser with click, type, scroll actions.
- `generate_citations` — rewrite markdown links as numbered citations with a reference list.
- `get_version` — report the crawlberg library version.

The CLI offers the same operations as subcommands — `scrape`, `crawl`, `map`, `interact`, `batch-scrape`, `batch-crawl`, `download`, `citations`, `version` — plus `serve` (a Firecrawl-v1-compatible REST API server) and `mcp` (the stdio MCP server). See the `crawlberg` skill for the full per-subcommand flag surface, and the `crawlberg-serving-the-api` skill for when to run the server instead of the CLI or MCP.

## Configuration

Pass flags or use inline JSON via `--config`:

```bash
crawlberg scrape https://example.com \
  --format markdown \
  --browser-mode auto \
  --timeout 30000
```

For complex configs, use JSON:

```bash
crawlberg crawl https://example.com \
  --config '{"max_depth":3,"max_pages":200,"max_concurrent":8,"respect_robots_txt":true}'
```

See the `crawlberg` and `crawlberg-crawling-a-site` skills for the full flag surface.

## Examples

Fetch a single page and print Markdown:

```text
crawlberg scrape https://example.com/article --format markdown
```

Crawl a site at depth 3 with rate limiting:

```text
crawlberg crawl https://example.com --depth 3 --max-pages 200 --concurrent 8 --stay-on-domain --format markdown
```

Enumerate URLs from a sitemap:

```text
crawlberg map https://example.com --limit 500
```

## Versioning

The plugin version tracks the crawlberg core release (`Cargo.toml`). See [CHANGELOG.md](../CHANGELOG.md) for release notes.

## License

MIT.

## See also

- **Marketplace**: this repo — `/plugin marketplace add xberg-io/crawlberg`
- **Upstream**: [xberg-io/crawlberg](https://github.com/xberg-io/crawlberg)

## Generated runtime bundles

`plugin/.ai-rulez/config.toml` loads `config.plugins.toml` through the `plugins` layer;
skills, commands and runtime adapters are authored alongside it. The latest AI-Rulez release (5.7.0 or newer) generates
all eleven supported runtimes: Claude, Cursor, Codex, Gemini, Kimi, OpenCode, Factory,
Hermes, Pi, Agent Plugins and Copilot. Codex ships both manifest layouts; repository-root
Claude and Codex marketplace indexes point to the `plugin` bundle. Cursor also has a marketplace index.

Pi packages skills and prompts without MCP integration. Copilot packages skills and MCP configuration, but does not package slash commands or hooks.
Hermes includes a Python package under `.hermes/package`; OpenCode and Pi share the npm package.
The Agent Plugins manifest uses the stable 1.0 specification. Runtime capabilities differ;
install the CLI before using skills in a runtime without MCP registration.

Run `ai-rulez generate --plugin` from the repository root to regenerate and
`ai-rulez verify --plugin` to compare current source output and provenance.
The release workflow packages every runtime into a deterministic archive, checksums,
and marketplace metadata; npm and PyPI retain their existing publishing jobs.

## Hermes runtime setup

Install the product CLI with MCP support and put it on Hermes' PATH before using the plugin.
The Hermes adapter registers every bundled skill under the product namespace and provides
`crawlberg_call`, which calls the selected native MCP tool with `tool_arguments` through a local
stdio session. Select `tool: "list_tools"` first to inspect the server's exact request schemas. Hermes skills are selected explicitly; they are not automatically appended to the
system prompt. Enable the plugin in Hermes configuration; project plugins also require
`HERMES_ENABLE_PROJECT_PLUGINS=true`.

The bridge uses the installed CLI directly. The auto-installing launchers used by other MCP clients
are not included in the Hermes Python wheel. Provider credentials are inherited from the environment,
and are needed for provider-backed requests. Missing CLIs, unavailable tools and model preparation
fail with an actionable request error. CLI result data remains untrusted document content.

## Public Agent Skills

The repository-root `skills/` directory contains the same generated skills and supporting
resources as `plugin/skills/`. Install them independently of a plugin runtime:

```sh
npx skills add xberg-io/crawlberg --skill crawlberg
```

Edit `plugin/.ai-rulez/skills`, then run `mise run plugin:generate` to regenerate runtime
bundles and synchronize the public export. `mise run plugin:verify` checks both projections;
`mise run plugin:skills:test` verifies resource freshness, ownership and symlink protection.
The public ownership manifest records SHA-256 hashes of every exported file.

Download integrity: the launcher currently relies on HTTPS for direct binary downloads.
Verify release checksums separately when available before manually installing an archive.

Launcher and visual assets are authored in `plugin/.ai-rulez/scripts/` and
`plugin/.ai-rulez/assets/`; `mise run plugin:generate` copies them into the runtime bundle.
