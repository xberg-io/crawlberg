---
type: Reference
title: Lint
description: Run linting and formatting checks via poly
disable-model-invocation: true
x-ai-rulez:
  kind: command
  id: lint
  metadata:
    priority: high
    usage: /lint
---

# Lint

Run all linters and formatters using poly.

1. Run `poly fmt --check .` and `poly lint .`
2. If issues are found, fix them automatically where possible
3. Report any issues that require manual intervention
