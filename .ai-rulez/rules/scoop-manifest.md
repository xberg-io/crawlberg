---
type: Reference
title: Scoop Manifest Layout
paths:
  - "scripts/publish/crawlberg.json.tmpl"
  - "scripts/publish/scoop.json"
  - ".github/workflows/publish.yaml"
x-ai-rulez:
  kind: rule
  id: scoop-manifest
  metadata:
    priority: medium
---

- The Windows zip nests the binary in a directory named after the asset, so `extract_dir` is `crawlberg-cli-x86_64-pc-windows-msvc`. The layout comes from `reusable-cli-release.yml` (`asset-prefix: crawlberg-cli-` in .github/workflows/publish.yaml), not from `scripts/ci/rust/package-cli-windows.ps1`, which builds a flat zip and is unused by publish.
