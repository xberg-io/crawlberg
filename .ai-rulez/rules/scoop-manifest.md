---
type: Reference
title: Scoop Manifest Layout
paths:
  - "scripts/publish/crawlberg.json.tmpl"
x-ai-rulez:
  kind: rule
  id: scoop-manifest
  metadata:
    priority: medium
---

- The Windows zip nests the binary in a directory named after the asset, so `extract_dir` is `crawlberg-cli-x86_64-pc-windows-msvc`.
- The template is rendered into the scoop-bucket manifest at release. A wrong `extract_dir` still hashes and validates but installs a shim that resolves to nothing; check `unzip -l` on the release asset after changing the archive layout.
