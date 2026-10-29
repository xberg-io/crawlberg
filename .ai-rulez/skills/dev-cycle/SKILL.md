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
mise run alef:install        # cargo install --path ../alef --locked
mise run alef:generate       # alef all --clean --skip-snippet-validation
mise run alef:format         # explicit Alef formatting; mise run format excludes this
mise run alef:bump 0.15.30   # bump the alef.toml and mise.toml pins + install + regen
```

- `alef:generate` has no `--skip-compile`, so it also compiles the Swift bridge and the FFI header
  gate (cargo over the whole dependency graph). A subagent without build scope must not run it.
- `alef:install` builds `../alef`, which exists only from a root checkout. From
  `.worktrees/crawlberg/<name>`, run `cargo install --path ../../../alef --locked` instead. Inside
  `mise run`, the alef pinned in `mise.toml` comes first on PATH; `alef:bump` moves that pin too.

## After crawlberg-core changes

```text
mise run build          # core-only
mise run build:bindings # all bindings
mise run build:all      # core + all bindings
```

## E2E cycles

```text
mise run e2e:generate # regenerate all language test suites from fixtures
mise run e2e:build    # rebuild the e2e mock-server only (bindings: mise run build:bindings)
mise run e2e:test     # run generated e2e suites
mise run e2e:all      # e2e:verify: generate + lint + test
```

## Cleanup

```text
mise run clean           # per-language artifacts
mise run clean:workspace # cargo target + alef IR cache + legacy mock-server bins
mise run clean:e2e       # venvs, node_modules, _build, lockfiles, vendor/
mise run clean:full      # all of the above
```

## Gotchas (codified, but worth knowing)

- **One mock-server binary.** `e2e/rust/src/main.rs` is alef-generated and is the binary all language conftests spawn. (An older `tools/mock-server` was removed; only the `clean` task description still mentions it.) `mise run e2e:build` builds the right one at `e2e/rust/target/release/mock-server`.
- **Python venv stale `.so`.** After `mise run python:build:dev`, the e2e venv at `e2e/python/.venv` keeps a stale extension. `mise run python:cycle` runs `uv sync --reinstall` to refresh.
- **Elixir precompiled NIF.** Rustler's precompiled binary is tagged at the package's release version and lags local source. `CRAWLBERG_BUILD=1` is set by the `elixir:e2e:test` task so the e2e suite always builds the NIF from local source.
- **Generated e2e files.** `e2e/<lang>/` is alef-generated — never hand-edit. Modify fixtures or alef codegen, then `mise run alef:generate`.
