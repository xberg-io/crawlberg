---
type: Reference
title: Review
description: Review current changes for correctness, style, and potential issues
x-ai-rulez:
  kind: command
  id: review
  metadata:
    priority: high
    usage: /review
---

# Review

Review all staged and unstaged changes in the current repository.

For each changed file:

1. Check for correctness and potential bugs
2. Verify adherence to project conventions and coding standards
3. Look for security issues (OWASP top 10, injection, secrets)
4. Check test coverage for new/modified code
5. Verify error handling is complete

Provide a concise summary of findings organized by severity (critical, warning, suggestion).
