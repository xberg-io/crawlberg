---
title: "AI Coding Assistants"
---

The Crawlberg plugin lets your coding agent crawl and scrape the web without leaving the chat — ask it to pull a page as Markdown, map a site, or extract metadata, and it drives Crawlberg for you.

**What the plugin does:** it bundles the Crawlberg agent skills (site crawling, HTML→Markdown scraping, headless-Chrome fallback) and wires up the `crawlberg` MCP server, so any major coding agent can call the crawler directly.

The plugin shells out to the `crawlberg` CLI. Install it from the [Installation](/getting-started/installation/) guide (for example, `brew install xberg-io/tap/crawlberg`) before driving the crawler from an assistant.

The plugin, its per-platform manifests, and version history live in the [`xberg-io/plugins`](https://github.com/xberg-io/plugins) marketplace.

:::note
The plugin registers the `crawlberg` MCP server for you. To configure or run that server standalone, see the [MCP Server guide](/guides/mcp-server/) and the [MCP reference](/reference/mcp/).
:::

## Installing

Pick your harness below.

<details open>
<summary><strong>Claude Code</strong></summary>

```text
/plugin marketplace add xberg-io/plugins
/plugin install crawlberg@xberg
```

</details>

<details>
<summary><strong>Codex CLI</strong></summary>

```text
/plugins add https://github.com/xberg-io/plugins
```

Then search for `crawlberg` and select **Install Plugin**.
</details>

<details>
<summary><strong>Cursor</strong></summary>

Settings → Plugins → Add from URL → `https://github.com/xberg-io/plugins`, then select **crawlberg**.
</details>

<details>
<summary><strong>Gemini CLI</strong></summary>

```text
gemini extensions install https://github.com/xberg-io/plugins
```

</details>

<details>
<summary><strong>Factory Droid</strong></summary>

```text
droid plugin marketplace add https://github.com/xberg-io/plugins
droid plugin install crawlberg@xberg
```

</details>

<details>
<summary><strong>GitHub Copilot CLI</strong></summary>

```text
copilot plugin marketplace add https://github.com/xberg-io/plugins
copilot plugin install crawlberg@xberg
```

</details>

<details>
<summary><strong>opencode</strong></summary>

Add the package to `opencode.json`:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "plugin": ["@xberg-io/opencode-crawlberg"]
}
```

</details>
