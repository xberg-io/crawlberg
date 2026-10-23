// AI-RULEZ :: GENERATED FILE — DO NOT EDIT
// Content-Hash: blake3:1acb0b9e6a7ba4f729e29dee8776ca8b65ab50c331851c7b60b7d20e4ff67cb5
// Source-Hash: blake3:45e0d62770afaf0c4706c19129d349dd2b9fa9bc0cf1d12d59c1ade364a14e34
// Schema-Version: v1

import { registerBundledContent } from "../ai-rulez-content.js";
import { spawn } from "node:child_process";

const outputFormat = { type: "string", enum: ["json", "markdown"], default: "json", description: "CLI output format." };
const browserMode = {
  type: "string",
  enum: ["auto", "always", "never"],
  default: "auto",
  description: "When to use headless browser fallback.",
};

function hasValue(value) {
  return value !== undefined && value !== null && value !== "";
}

function pushOption(args, name, value) {
  if (hasValue(value)) {
    args.push(name, String(value));
  }
}

function pushFlag(args, name, enabled) {
  if (enabled) {
    args.push(name);
  }
}

function validateJson(value, name) {
  if (!hasValue(value)) {
    return;
  }

  try {
    JSON.parse(value);
  } catch (error) {
    throw new Error(`${name} must be valid JSON: ${error.message}`, { cause: error });
  }
}

function runCli(args, context) {
  const directory = context?.directory ?? context?.worktree ?? process.cwd();

  return new Promise((resolve, reject) => {
    const child = spawn("crawlberg", args, {
      cwd: directory,
      env: process.env,
      signal: context?.signal,
      stdio: ["ignore", "pipe", "pipe"],
    });

    const stdout = [];
    const stderr = [];

    child.stdout.on("data", (chunk) => stdout.push(chunk));
    child.stderr.on("data", (chunk) => stderr.push(chunk));
    child.on("error", (error) => {
      if (error.code === "ENOENT") {
        resolve({
          title: "crawlberg CLI not found",
          content:
            "Install the crawlberg CLI on PATH with `brew install xberg-io/tap/crawlberg` or `cargo install crawlberg-cli --features all`.",
          metadata: { exitCode: 127, command: "crawlberg", subcommand: args[0] },
        });
        return;
      }
      reject(error);
    });
    child.on("close", (exitCode, signal) => {
      const stdoutText = Buffer.concat(stdout).toString("utf8").trim();
      const stderrText = Buffer.concat(stderr).toString("utf8").trim();
      const output = [stdoutText, stderrText && `stderr:\n${stderrText}`].filter(Boolean).join("\n\n");

      resolve({
        title: exitCode === 0 ? `crawlberg ${args[0]}` : `crawlberg ${args[0]} failed`,
        content: output || "(no output)",
        metadata: {
          exitCode,
          signal,
          command: "crawlberg",
          subcommand: args[0],
        },
      });
    });
  });
}

function pushSharedCrawlOptions(cliArgs, args) {
  pushOption(cliArgs, "--format", args.format);
  pushOption(cliArgs, "--timeout", args.timeout);
  pushOption(cliArgs, "--browser-mode", args.browser_mode);
  pushOption(cliArgs, "--browser-endpoint", args.browser_endpoint);
  pushOption(cliArgs, "--user-agent", args.user_agent);
  pushOption(cliArgs, "--proxy", args.proxy);
  pushFlag(cliArgs, "--respect-robots-txt", args.respect_robots_txt);
  pushOption(cliArgs, "--config", args.config);
}

export const CrawlbergPlugin = {
  id: "crawlberg",
  async setup(ctx) {
    await registerBundledContent(ctx);
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "crawlberg_scrape",
        description: "Scrape one URL to JSON or Markdown with the crawlberg CLI.",
        input: {
          type: "object",
          properties: {
            url: { type: "string", format: "uri", description: "URL to scrape." },
            format: outputFormat,
            timeout: {
              type: "integer",
              minimum: 1,
              maximum: 600000,
              default: 30000,
              description: "Request timeout in ms.",
            },
            browser_mode: browserMode,
            browser_endpoint: { type: "string", format: "uri", description: "Optional CDP WebSocket endpoint." },
            user_agent: { type: "string", minLength: 1, description: "Optional HTTP user agent." },
            proxy: { type: "string", format: "uri", description: "Optional proxy URL." },
            respect_robots_txt: { type: "boolean", default: false, description: "Respect robots.txt." },
            config: { type: "string", minLength: 2, description: "Optional CrawlConfig JSON." },
          },
          required: ["url"],
          additionalProperties: false,
        },
        async execute(args, context) {
          validateJson(args.config, "config");

          const cliArgs = ["scrape", args.url];
          pushSharedCrawlOptions(cliArgs, args);
          return await runCli(cliArgs, { ...context, directory: ctx.location.directory });
        },
      });
      editor.add({
        name: "crawlberg_crawl",
        description: "Crawl one or more seed URLs to JSON or Markdown with the crawlberg CLI.",
        input: {
          type: "object",
          properties: {
            urls: {
              type: "array",
              items: { type: "string", format: "uri" },
              minItems: 1,
              description: "Seed URLs to crawl.",
            },
            depth: { type: "integer", minimum: 0, maximum: 20, default: 2, description: "Maximum crawl depth." },
            max_pages: { type: "integer", minimum: 1, description: "Maximum pages to crawl." },
            concurrent: {
              type: "integer",
              minimum: 1,
              maximum: 256,
              default: 10,
              description: "Maximum concurrent requests.",
            },
            rate_limit: { type: "integer", minimum: 0, default: 200, description: "Delay between requests in ms." },
            stay_on_domain: { type: "boolean", default: false, description: "Restrict crawling to the seed domain." },
            format: outputFormat,
            timeout: {
              type: "integer",
              minimum: 1,
              maximum: 600000,
              default: 30000,
              description: "Request timeout in ms.",
            },
            browser_mode: browserMode,
            browser_endpoint: { type: "string", format: "uri", description: "Optional CDP WebSocket endpoint." },
            user_agent: { type: "string", minLength: 1, description: "Optional HTTP user agent." },
            proxy: { type: "string", format: "uri", description: "Optional proxy URL." },
            respect_robots_txt: { type: "boolean", default: false, description: "Respect robots.txt." },
            config: { type: "string", minLength: 2, description: "Optional CrawlConfig JSON." },
          },
          required: ["urls"],
          additionalProperties: false,
        },
        async execute(args, context) {
          validateJson(args.config, "config");

          const cliArgs = [
            "crawl",
            ...args.urls,
            "--depth",
            String(args.depth ?? 2),
            "--concurrent",
            String(args.concurrent ?? 10),
          ];
          pushOption(cliArgs, "--max-pages", args.max_pages);
          pushOption(cliArgs, "--rate-limit", args.rate_limit);
          pushFlag(cliArgs, "--stay-on-domain", args.stay_on_domain);
          pushSharedCrawlOptions(cliArgs, args);
          return await runCli(cliArgs, { ...context, directory: ctx.location.directory });
        },
      });
      editor.add({
        name: "crawlberg_map",
        description: "Enumerate URLs from sitemaps and link extraction with the crawlberg CLI.",
        input: {
          type: "object",
          properties: {
            url: { type: "string", format: "uri", description: "URL to map." },
            limit: { type: "integer", minimum: 1, description: "Maximum URLs to return." },
            search: { type: "string", minLength: 1, description: "Filter URLs by substring." },
            format: outputFormat,
            timeout: {
              type: "integer",
              minimum: 1,
              maximum: 600000,
              default: 30000,
              description: "Request timeout in ms.",
            },
            browser_mode: browserMode,
            browser_endpoint: { type: "string", format: "uri", description: "Optional CDP WebSocket endpoint." },
            respect_robots_txt: { type: "boolean", default: false, description: "Respect robots.txt." },
            config: { type: "string", minLength: 2, description: "Optional CrawlConfig JSON." },
          },
          required: ["url"],
          additionalProperties: false,
        },
        async execute(args, context) {
          validateJson(args.config, "config");

          const cliArgs = ["map", args.url];
          pushOption(cliArgs, "--limit", args.limit);
          pushOption(cliArgs, "--search", args.search);
          pushOption(cliArgs, "--format", args.format);
          pushOption(cliArgs, "--timeout", args.timeout);
          pushOption(cliArgs, "--browser-mode", args.browser_mode);
          pushOption(cliArgs, "--browser-endpoint", args.browser_endpoint);
          pushFlag(cliArgs, "--respect-robots-txt", args.respect_robots_txt);
          pushOption(cliArgs, "--config", args.config);
          return await runCli(cliArgs, { ...context, directory: ctx.location.directory });
        },
      });
    });
  },
};

export default CrawlbergPlugin;
