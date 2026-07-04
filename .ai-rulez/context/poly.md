---
priority: high
---

# poly

poly (polylint) is a single-binary, multi-language linter and formatter. It bundles engines (ruff for Python, oxc for JS/TS/JSON, taplo for TOML, rumdl for Markdown) and delegates to native tools (cargo fmt/clippy, golangci-lint, actionlint, shellcheck, shfmt) when present, so most repos need no extra toolchains.

## Commands

- Lint: `poly lint .`
- Check formatting (dry-run): `poly fmt --check .`
- Apply formatting: `poly fmt --fix .`
- Apply lint autofixes: `poly lint --fix .`

## Configuration

Per-repo `poly.toml` — `[discovery]` excludes, `[lint.<lang>.<tool>]` rules, `[per-file-ignores]`, `[fmt.<lang>.<tool>]` options. Cache dir `.polylint/` (gitignored).

## Severity

`poly lint` exits non-zero only on error-severity findings; warnings are reported but don't fail CI.

## CI

Validation runs via the shared reusable workflow: `uses: xberg-io/actions/.github/workflows/reusable-validate.yml@v1` (runs `poly fmt --check .` then `poly lint .`).

Run `poly fmt --check .` and `poly lint .` after changes to verify compliance.
