---
priority: critical
---

- All files in e2e/ are generated — DO NOT EDIT, they include a generated-code header
- To change: modify fixtures or generator source, run `task e2e:generate`, run `task e2e:test`, commit together
- CI drift gate: the `e2e-tests` matrix in `.github/workflows/ci-e2e.yaml` runs `alef e2e generate --lang <lang>` and `git diff --exit-code` against that language's `e2e/` directory, for every matrix language except `python` and `php`. Those two are deliberately excluded — alef's format step for `python` shells out to a bare `ruff` binary (not `uv run ruff`, and ruff is not a project dependency anywhere in this repo) and for `php` requires `packages/php/vendor/bin/php-cs-fixer`, which this CI job never installs — so regenerating them here would produce unformatted output that false-positives against the committed, formatted tree. See the `~keep` comment above the check in the workflow for the full per-language toolchain reasoning.
