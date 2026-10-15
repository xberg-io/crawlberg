---
title: "AI Coding Assistants"
---

The Crawlberg plugin lets your coding agent crawl and scrape the web without leaving the chat — ask it to pull a page as Markdown, map a site, or extract metadata, and it drives Crawlberg for you.

**What the plugin does:** it bundles the Crawlberg agent skills (site crawling, HTML→Markdown scraping, headless-Chrome fallback) and wires up the `crawlberg` MCP server, so any major coding agent can call the crawler directly.

The plugin shells out to the `crawlberg` CLI. Install it from the [Installation](/getting-started/installation/) guide (for example, `brew install xberg-io/tap/crawlberg`) before driving the crawler from an assistant.

The plugin, its per-platform manifests, and version history live in this repository's self-hosted [`xberg-io/crawlberg`](https://github.com/xberg-io/crawlberg) marketplace.

:::note
MCP-capable integrations register the `crawlberg` MCP server for you. Pi uses skills and CLI access instead. To configure or run that server standalone, see the [MCP Server guide](/guides/mcp-server/) and the [MCP reference](/reference/mcp/).
:::

## Installing

Pick your harness below.

<details open>
<summary><strong>Claude Code</strong></summary>

```text
/plugin marketplace add xberg-io/crawlberg
/plugin install crawlberg@crawlberg
```

</details>

<details>
<summary><strong>Codex CLI</strong></summary>

```text
/plugins add https://github.com/xberg-io/crawlberg
```

Then search for `crawlberg` and select **Install Plugin**.
</details>

<details>
<summary><strong>Cursor</strong></summary>

Settings → Plugins → Add from URL → `https://github.com/xberg-io/crawlberg`, then select **crawlberg**.
</details>

<details>
<summary><strong>Gemini CLI</strong></summary>

From a checkout of this repository:

```sh
gemini extensions install ./plugin
```

The manifest is inside `plugin/`. See the [Gemini extension reference](https://geminicli.com/docs/extensions/reference/)
for local installation and updates.

</details>

<details>
<summary><strong>Kimi installation and compatibility</strong></summary>

From a checkout of this repository, install with the Python Kimi CLI version used for native
plugin validation:

```sh
uvx --from kimi-cli==1.51.0 kimi plugin install ./plugin
```

Native plugin installation was validated with Python Kimi CLI 1.51.0. The replacement
`@moonshot-ai/kimi-code` 2.1.1 does not provide a `plugin` CLI subcommand; its interactive
plugin loading has not been verified. These results do not establish compatibility with
newer Kimi releases.

</details>

<details>
<summary><strong>Factory Droid</strong></summary>

```text
droid plugin marketplace add https://github.com/xberg-io/crawlberg
droid plugin install crawlberg@crawlberg
```

</details>

<details>
<summary><strong>GitHub Copilot CLI</strong></summary>

```text
copilot plugin marketplace add https://github.com/xberg-io/crawlberg
copilot plugin install crawlberg@crawlberg
```

</details>

<details>
<summary><strong>Pi</strong></summary>

From a checkout of this repository:

```bash
pi install ./plugin
```

After the next plugin release, install the same package used by OpenCode:

```bash
pi install npm:@xberg-io/opencode-crawlberg
```

Pi loads the bundled skills and prompt templates. Use `/crawlberg-scrape` or `/crawlberg-crawl` with the input as arguments. Install the `crawlberg` CLI separately for shell access. This package does not register MCP servers, hooks, or native Pi extensions.

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

## Runtime resources and prerequisites

The generated bundle targets Claude Code, Codex, Cursor, Gemini CLI, Kimi, OpenCode,
Factory Droid, Hermes, Pi, Agent Plugins and GitHub Copilot CLI. Use
[the plugin installation guide](https://github.com/xberg-io/crawlberg/blob/main/plugin/README.md)
for the runtime's installation method and configuration.

| Runtime | Integration |
| --- | --- |
| Claude, Codex, Cursor, Factory | Native plugin manifests, skills and MCP configuration |
| Gemini CLI | Extension manifest, command templates and MCP configuration |
| Kimi | Plugin manifest and session instructions |
| OpenCode | npm adapter, instructions and MCP configuration |
| Hermes | Python plugin, explicitly selected skills and a local stdio MCP bridge |
| Pi | Skills and prompt templates; execute operations through the CLI |
| Agent Plugins, Copilot | Agent Plugins 1.0 manifest, skills and MCP configuration |

Install the product CLI before using Hermes or Pi. Hermes starts the installed CLI directly;
its wheel does not include the auto-installing launcher used by other MCP clients.
Select `list_tools` through the product's Hermes bridge to inspect the supported tools and
request schemas. Select skills explicitly when needed; they are not appended automatically
to every conversation.

From a checkout, install the generated Hermes package into Hermes' Python environment:

```sh
pip install ./plugin/.hermes/package
```

Enable the plugin in Hermes configuration. Loading project plugins also requires
`HERMES_ENABLE_PROJECT_PLUGINS=true`. After a release publishes the updated package, the
registry install is `pip install crawlberg-hermes-plugin`.

Pi resources can be installed from the checkout with `pi install ./plugin`.
After a plugin release includes them, install `npm:@xberg-io/opencode-crawlberg` through Pi.
Pi does not register an MCP server, hooks or executable extensions from this package.

## Standalone public skills

The [public skills directory](https://github.com/xberg-io/crawlberg/tree/main/skills) contains
instructions you can install independently of the runtime plugin. Use the
[skills CLI](https://github.com/vercel-labs/skills) to list and select them:

```sh
npx skills add xberg-io/crawlberg --list
npx skills add xberg-io/crawlberg
```

Choose the target agents interactively. For a local checkout, use `npx skills add ./skills`.
This installs instructions; it does not install the product CLI, enable MCP, download models
or set provider credentials.
