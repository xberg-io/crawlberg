---
type: Reference
title: Test
description: Run the project test suite and report results
x-ai-rulez:
  kind: command
  id: test
  metadata:
    priority: high
    usage: /test
---

# Test

Run the project's test suite using the standard task runner.

1. Run `task test` to execute all tests
2. If tests fail, analyze the failures and suggest fixes
3. Report a summary: total tests, passed, failed, skipped
