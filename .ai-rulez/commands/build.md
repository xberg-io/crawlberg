---
type: Reference
title: Build
description: Build the project and verify compiled artifacts
x-ai-rulez:
  kind: command
  id: build
  metadata:
    priority: medium
    usage: /build
---

# Build

Build the project and verify compiled artifacts using the standard task runner.

1. Run `task build` for the core build
2. Run `task build:bindings` or `task build:all` explicitly when bindings are needed
3. Report any build errors with context
4. Suggest fixes for any compilation failures
