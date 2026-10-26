---
type: Concept
title: Area Map
x-ai-rulez:
  kind: context
  id: area-map
  metadata:
    priority: high
---

# Area Map

Crawlberg is a Rust web crawling and scraping library with polyglot bindings.

- `crates/crawlberg/` — core engine (`crawl-engine-map` skill)
- `crates/crawlberg-{browser,bypass,robots}/` — browser fallback, bypass, robots.txt
- `crates/crawlberg-cli/` — command-line binary
- `crates/crawlberg-{py,node,php,wasm,ffi,jni}/` — binding crates
- `packages/<lang>/` — generated language packages
- `e2e/<lang>/`, `fixtures/` — generated e2e suites, fixtures (`generated-code-policy` rule)
- `test_apps/<lang>/` — generated suites for published packages
- `tests/api/`, `fuzz/`, `tools/` — API contract tests, fuzzing, benchmarks
- `vendor/` — vendored browser driver fork
- `docs-site/`, `readme_templates/`, `templates/` — docs and templates
- `skills/`, `plugin/` — published skills, agent plugin
- `docker/`, `scripts/` — images, CI scripts

Build, regen and e2e loops: `dev-cycle` skill.
