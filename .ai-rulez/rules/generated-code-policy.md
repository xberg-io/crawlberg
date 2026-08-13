---
priority: critical
---

- Almost everything in e2e/ is generated — DO NOT EDIT. The generated files are the ones carrying an `alef:hash:` provenance line in their header; that marker, not the directory, is what identifies generator-owned content
- Six files under e2e/ are HAND-WRITTEN and must never be regenerated over or deleted: `e2e/node/tests/ssrf.test.ts`, `e2e/wasm/tests/ssrf.test.ts`, `e2e/rust/tests/ssrf_test.rs` (the SSRF-defence coverage, each self-labelled in its first line), plus `e2e/go/helpers_test.go`, `e2e/go/main_test.go`, and `e2e/elixir/test/test_helper.exs`. They carry no marker, so nothing automated distinguishes them from generator output — treat this list as the record
- To change: modify fixtures or generator source, run `task e2e:generate`, run `task e2e:test`, commit together
- CI drift gate: the `e2e-tests` matrix in `.github/workflows/ci-e2e.yaml` runs `alef e2e generate --lang <lang>` and `git diff --exit-code` against that language's `e2e/` directory, for every matrix language except `python` and `php`. Those two are deliberately excluded — alef's format step for `python` shells out to a bare `ruff` binary (not `uv run ruff`, and ruff is not a project dependency anywhere in this repo) and for `php` requires `packages/php/vendor/bin/php-cs-fixer`, which this CI job never installs — so regenerating them here would produce unformatted output that false-positives against the committed, formatted tree. See the `~keep` comment above the check in the workflow for the full per-language toolchain reasoning.
