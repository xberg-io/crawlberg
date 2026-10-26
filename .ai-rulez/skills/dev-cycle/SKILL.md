---
type: Playbook
title: Dev Cycle
description: Crawlberg Taskfile loops for alef regeneration, core and binding builds, e2e generate/build/test and cleanup, plus known e2e gotchas. Use when unsure which task to run, or when a crawlberg build, alef regen or e2e run misbehaves.
x-ai-rulez:
  kind: skill
  id: dev-cycle
  metadata:
    name: dev-cycle
---

Iteration loops are codified as Taskfile tasks. Prefer them over ad-hoc commands.

## After alef changes

```text
task alef:install              # cargo install --path ../alef/crates/alef-cli
task alef:generate             # alef all --clean --format=false
task alef:format               # explicit Alef formatting; task format excludes this
VERSION=0.15.30 task alef:bump # bump pin in alef.toml + reinstall + regen
```

## After crawlberg-core changes

```text
task build          # core-only
task build:bindings # all bindings
task build:all      # core + all bindings
```

## E2E cycles

```text
task e2e:generate # regenerate all language test suites from fixtures
task e2e:build    # build bindings and the generated mock-server for e2e
task e2e:test     # run generated e2e suites
task e2e:all      # generate + build + run generated e2e suites
```

## Cleanup

```text
task clean           # per-language artifacts
task clean:workspace # cargo target + alef IR cache + legacy mock-server bins
task clean:e2e       # venvs, node_modules, _build, lockfiles, vendor/
task clean:full      # all of the above
```

## Gotchas (codified, but worth knowing)

- **One mock-server binary.** `e2e/rust/src/main.rs` is alef-generated and is the binary all language conftests spawn. (An older `tools/mock-server` was removed; only the `task clean` description still mentions it.) `task e2e:build` builds the right one at `e2e/rust/target/release/mock-server`.
- **Python venv stale `.so`.** After `task python:build:dev`, the e2e venv at `e2e/python/.venv` keeps a stale extension. `task python:cycle` runs `uv sync --reinstall` to refresh.
- **Elixir precompiled NIF.** Rustler's precompiled binary is tagged at the package's release version and lags local source. `CRAWLBERG_BUILD=1` is set in `.task/languages/elixir.yml::e2e:test` so the e2e suite always builds the NIF from local source.
- **Generated e2e files.** `e2e/<lang>/` is alef-generated — never hand-edit. Modify fixtures or alef codegen, then `task alef:generate`.
