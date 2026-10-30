---
type: Playbook
title: Dev Cycle
description: Crawlberg mise task loops for alef regeneration, core and binding builds, e2e generate/build/test and cleanup, plus known e2e gotchas. Use when unsure which task to run, or when a crawlberg build, alef regen or e2e run misbehaves.
x-ai-rulez:
  kind: skill
  id: dev-cycle
  metadata:
    name: dev-cycle
---

Iteration loops are codified as mise tasks (`mise tasks` lists them). Prefer them over ad-hoc commands.

## After alef changes

```text
alef all --clean --skip-snippet-validation # regenerate all Alef-managed output
mise run alef:verify                       # check the generated files are up to date
```

- `alef all` has no `--skip-compile`, so it also compiles the Swift bridge and the FFI header
  gate (cargo over the whole dependency graph). A subagent without build scope must not run it.
- To move the alef pin, change `alef_version` in `alef.toml` and `"github:xberg-io/alef"` in
  `mise.toml` together: inside `mise run`, the alef pinned in `mise.toml` comes first on PATH.

## After crawlberg-core changes

```text
mise run build      # core-only
mise run alef:build # all bindings
```

## E2E cycles

```text
mise run e2e:generate     # regenerate all language test suites from fixtures
mise run e2e:build        # rebuild the e2e mock-server only (bindings: mise run alef:build)
mise run <lang>:e2e:test  # run one generated e2e suite, as CI does per language
```

## Cleanup

```text
mise run clean # per-language artifacts
```

## Gotchas (codified, but worth knowing)

- **One mock-server binary.** `e2e/rust/src/main.rs` is alef-generated and is the binary all language conftests spawn. `mise run e2e:build` builds the right one at `e2e/rust/target/release/mock-server`.
- **Python venv stale `.so`.** After rebuilding the extension (`uv run maturin develop` in `packages/python`), the e2e venv at `e2e/python/.venv` keeps a stale extension. `cd e2e/python && uv sync --reinstall` refreshes it.
- **Elixir precompiled NIF.** Rustler's precompiled binary is tagged at the package's release version and lags local source. `CRAWLBERG_BUILD=1` is set by the `elixir:e2e:test` task so the e2e suite always builds the NIF from local source.
- **Generated e2e files.** `e2e/<lang>/` is alef-generated — never hand-edit. Modify fixtures or alef codegen, then rerun `alef all --clean --skip-snippet-validation`.
