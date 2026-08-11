// AI-RULEZ :: GENERATED FILE — DO NOT EDIT
// Content-Hash: blake3:ce1f9c841aabffb21311dce8d26479b2d2bba538a24ace5ed6246a918cc50429
// Source-Hash: blake3:38437ea4e4b834a49e1a4a00623bb764ea2f716d6f09c9b27cae5c187d86f764
// Schema-Version: v1

import {tool} from "@opencode-ai/plugin";
import {spawn} from "node:child_process";

const schema = tool.schema;

const outputFormat = schema.enum([ "json", "markdown" ])
                         .default("json")
                         .describe("CLI output format.");
const browserMode = schema.enum([ "auto", "always", "never" ])
                        .default("auto")
                        .describe("When to use headless browser fallback.");

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
    throw new Error(`${name} must be valid JSON: ${error.message}`);
  }
}

function runCli(args, context) {
  const directory = context?.directory ?? context?.worktree ?? process.cwd();

  return new Promise((resolve, reject) => {
    const child = spawn("crawlberg", args, {
      cwd : directory,
      env : process.env,
      signal : context?.abort,
      stdio : [ "ignore", "pipe", "pipe" ],
    });

    const stdout = [];
    const stderr = [];

    child.stdout.on("data", (chunk) => stdout.push(chunk));
    child.stderr.on("data", (chunk) => stderr.push(chunk));
    child.on("error", (error) => {
      if (error.code === "ENOENT") {
        resolve({
          title : "crawlberg CLI not found",
          output :
              "Install the crawlberg CLI with `brew install xberg-io/tap/crawlberg`, or run it via `npx @xberg-io/crawlberg-cli` / `uvx --from crawlberg-cli crawlberg`.",
          metadata :
              {exitCode : 127, command : "crawlberg", subcommand : args[0]},
        });
        return;
      }
      reject(error);
    });
    child.on("close", (exitCode, signal) => {
      const stdoutText = Buffer.concat(stdout).toString("utf8").trim();
      const stderrText = Buffer.concat(stderr).toString("utf8").trim();
      const output = [
        stdoutText, stderrText && `stderr:\n${stderrText}`
      ].filter(Boolean).join("\n\n");

      resolve({
        title : exitCode === 0 ? `crawlberg ${args[0]}`
                               : `crawlberg ${args[0]} failed`,
        output : output || "(no output)",
        metadata : {
          exitCode,
          signal,
          command : "crawlberg",
          subcommand : args[0],
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

export const CrawlbergPlugin = async () => ({
  tool : {
    crawlberg_scrape : tool({
      description :
          "Scrape one URL to JSON or Markdown with the crawlberg CLI.",
      args : {
        url : schema.string().url().describe("URL to scrape."),
        format : outputFormat,
        timeout : schema.number()
                      .int()
                      .positive()
                      .max(600000)
                      .default(30000)
                      .describe("Request timeout in ms."),
        browser_mode : browserMode,
        browser_endpoint : schema.string().url().optional().describe(
            "Optional CDP WebSocket endpoint."),
        user_agent : schema.string().min(1).optional().describe(
            "Optional HTTP user agent."),
        proxy :
            schema.string().url().optional().describe("Optional proxy URL."),
        respect_robots_txt :
            schema.boolean().default(false).describe("Respect robots.txt."),
        config : schema.string().min(2).optional().describe(
            "Optional CrawlConfig JSON."),
      },
      async execute(args, context) {
        validateJson(args.config, "config");

        const cliArgs = [ "scrape", args.url ];
        pushSharedCrawlOptions(cliArgs, args);
        return runCli(cliArgs, context);
      },
    }),
    crawlberg_crawl : tool({
      description :
          "Crawl one or more seed URLs to JSON or Markdown with the crawlberg CLI.",
      args : {
        urls : schema.array(schema.string().url())
                   .min(1)
                   .describe("Seed URLs to crawl."),
        depth : schema.number().int().min(0).max(20).default(2).describe(
            "Maximum crawl depth."),
        max_pages : schema.number().int().positive().optional().describe(
            "Maximum pages to crawl."),
        concurrent :
            schema.number().int().positive().max(256).default(10).describe(
                "Maximum concurrent requests."),
        rate_limit : schema.number().int().min(0).default(200).describe(
            "Delay between requests in ms."),
        stay_on_domain : schema.boolean().default(false).describe(
            "Restrict crawling to the seed domain."),
        format : outputFormat,
        timeout : schema.number()
                      .int()
                      .positive()
                      .max(600000)
                      .default(30000)
                      .describe("Request timeout in ms."),
        browser_mode : browserMode,
        browser_endpoint : schema.string().url().optional().describe(
            "Optional CDP WebSocket endpoint."),
        user_agent : schema.string().min(1).optional().describe(
            "Optional HTTP user agent."),
        proxy :
            schema.string().url().optional().describe("Optional proxy URL."),
        respect_robots_txt :
            schema.boolean().default(false).describe("Respect robots.txt."),
        config : schema.string().min(2).optional().describe(
            "Optional CrawlConfig JSON."),
      },
      async execute(args, context) {
        validateJson(args.config, "config");

        const cliArgs = [
          "crawl", ...args.urls, "--depth", String(args.depth), "--concurrent",
          String(args.concurrent)
        ];
        pushOption(cliArgs, "--max-pages", args.max_pages);
        pushOption(cliArgs, "--rate-limit", args.rate_limit);
        pushFlag(cliArgs, "--stay-on-domain", args.stay_on_domain);
        pushSharedCrawlOptions(cliArgs, args);
        return runCli(cliArgs, context);
      },
    }),
    crawlberg_map : tool({
      description :
          "Enumerate URLs from sitemaps and link extraction with the crawlberg CLI.",
      args : {
        url : schema.string().url().describe("URL to map."),
        limit : schema.number().int().positive().optional().describe(
            "Maximum URLs to return."),
        search : schema.string().min(1).optional().describe(
            "Filter URLs by substring."),
        format : outputFormat,
        timeout : schema.number()
                      .int()
                      .positive()
                      .max(600000)
                      .default(30000)
                      .describe("Request timeout in ms."),
        browser_mode : browserMode,
        browser_endpoint : schema.string().url().optional().describe(
            "Optional CDP WebSocket endpoint."),
        respect_robots_txt :
            schema.boolean().default(false).describe("Respect robots.txt."),
        config : schema.string().min(2).optional().describe(
            "Optional CrawlConfig JSON."),
      },
      async execute(args, context) {
        validateJson(args.config, "config");

        const cliArgs = [ "map", args.url ];
        pushOption(cliArgs, "--limit", args.limit);
        pushOption(cliArgs, "--search", args.search);
        pushOption(cliArgs, "--format", args.format);
        pushOption(cliArgs, "--timeout", args.timeout);
        pushOption(cliArgs, "--browser-mode", args.browser_mode);
        pushOption(cliArgs, "--browser-endpoint", args.browser_endpoint);
        pushFlag(cliArgs, "--respect-robots-txt", args.respect_robots_txt);
        pushOption(cliArgs, "--config", args.config);
        return runCli(cliArgs, context);
      },
    }),
  },
});

export default CrawlbergPlugin;
