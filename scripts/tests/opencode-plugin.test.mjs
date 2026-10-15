import assert from "node:assert/strict";
import test from "node:test";
import { mkdtemp, writeFile, rm, realpath } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import plugin from "../../plugin/.opencode/plugins/crawlberg.js";

async function setup(directory) {
  const records = { tool: [], skill: [], command: [], mcp: [] };
  const ctx = { location: { directory }, session: {} };
  for (const kind of Object.keys(records)) {
    ctx[kind] = {
      transform: (callback) =>
        callback({
          get: () => undefined,
          add: (value) => records[kind].push(value),
          set: (name, value) => records[kind].push({ name, ...value }),
        }),
    };
  }
  await plugin.setup(ctx);
  return records;
}

test("should_register_tools_skills_commands_and_mcp_from_installed_bundle", async () => {
  assert.equal(plugin.id, "crawlberg");
  const records = await setup(process.cwd());
  assert.deepEqual(
    records.tool.map((tool) => tool.name),
    ["crawlberg_scrape", "crawlberg_crawl", "crawlberg_map"],
  );
  assert.equal(records.skill.length, 7);
  assert.deepEqual(
    records.command.map((command) => command.name),
    ["crawlberg-crawl", "crawlberg-scrape"],
  );
  assert.equal(records.mcp.length, 1);
});

test("should_preserve_cli_arguments_output_working_directory_and_abort", async () => {
  const directory = await realpath(await mkdtemp(path.join(tmpdir(), "crawlberg-plugin-test-")));
  const previousPath = process.env.PATH;
  try {
    await writeFile(
      path.join(directory, "crawlberg"),
      `#!/usr/bin/env node
if (process.argv.includes('wait')) setInterval(() => {}, 1000);
else console.log(JSON.stringify({ args: process.argv.slice(2), cwd: process.cwd() }));
`,
      { mode: 0o755 },
    );
    process.env.PATH = `${directory}${path.delimiter}${previousPath}`;
    const records = await setup(directory);
    const result = await records.tool[0].execute(
      { url: "https://example.com", format: "markdown", respect_robots_txt: true },
      {},
    );
    assert.deepEqual(JSON.parse(result.content), {
      args: ["scrape", "https://example.com", "--format", "markdown", "--respect-robots-txt"],
      cwd: directory,
    });
    assert.equal(result.metadata.exitCode, 0);
    const crawl = await records.tool[1].execute(
      {
        urls: ["https://example.com", "https://example.org"],
        depth: 0,
        concurrent: 1,
        rate_limit: 0,
        stay_on_domain: true,
      },
      {},
    );
    assert.deepEqual(JSON.parse(crawl.content).args, [
      "crawl",
      "https://example.com",
      "https://example.org",
      "--depth",
      "0",
      "--concurrent",
      "1",
      "--rate-limit",
      "0",
      "--stay-on-domain",
    ]);
    const map = await records.tool[2].execute({ url: "https://example.com", limit: 5, search: "docs" }, {});
    assert.deepEqual(JSON.parse(map.content).args, ["map", "https://example.com", "--limit", "5", "--search", "docs"]);

    await assert.rejects(
      records.tool[0].execute({ url: "https://example.com", config: "bad-json" }, {}),
      /config must be valid JSON/,
    );
    const controller = new AbortController();
    const pending = records.tool[0].execute({ url: "wait" }, { signal: controller.signal });
    controller.abort();
    await assert.rejects(pending, { name: "AbortError" });
  } finally {
    process.env.PATH = previousPath;
    await rm(directory, { recursive: true, force: true });
  }
});
