---
status: accepted
date: 2026-10-30
decision-makers: ["Na'aman Hirschfeld"]
---

# Record architecture decisions

## Context and Problem Statement

Crawlberg kept Nygard-style records under `docs/adr/` from its first commit (5a34a5a363, 2026-03-09),
nine of them by the time they were deleted with the move to a documentation site (a8f6ab7068, 2026-04-09). Since then the
reasons behind its architecture live only in commit messages, code comments and docs pages, and nothing
marks when a decision changed. The xberg-io organization now records decisions as MADR files checked by
adrlint, in an org log and one log per repository.

## Considered Options

* A repository log of MADR records in `adrs/`, checked by adrlint, citing the org log for org-wide decisions
* Restore the deleted Nygard records under `docs/adr/`

## Decision Outcome

Chosen option: "A repository log of MADR records in `adrs/`, checked by adrlint, citing the org log for
org-wide decisions", because it is the format every other xberg-io log uses, and adrlint can check its
shape, numbering and, once locked, the immutability of accepted records.

Records follow MADR 4 as `adrlint new` writes it, numbered from 0000. ADRs 0001 to 0006 are seeds: decisions
already in force, dated to the commit that made them. Where a deleted `docs/adr/` record gave the rationale or
the alternatives, a seed cites it by path at a8f6ab7068's parent. Org-wide decisions are not restated here; see
the [org log](https://github.com/xberg-io/agent-conventions/tree/main/adrs), in particular
[org ADR-0002 (Rust cores with alef-generated bindings)](https://github.com/xberg-io/agent-conventions/blob/main/adrs/0002-rust-cores-with-alef-generated-bindings.md)
and [org ADR-0011 (Shared Cargo feature flag names)](https://github.com/xberg-io/agent-conventions/blob/main/adrs/0011-shared-cargo-feature-flag-names.md).
The index is the docs-site page `docs-site/src/content/docs/reference/decision-records.md`, outside the log.

### Consequences

* Good, because a later change to one of these decisions supersedes or amends a record instead of drifting.
* Good, because adrlint checks every record in CI.
* Bad, because the seeds are written after the fact, so their rationale is only as complete as the commits
  and deleted records they cite.

### Confirmation

`.adrlint.toml` sets `dir = "adrs"` and `start = 0`. `adrlint lint`, `adrlint format --check` and
`adrlint lock --check` run as the `adrlint` hooks declared in `poly.toml:887-895`, and in CI through
`check-adrs: true` on the `validate` job (`.github/workflows/ci-lint.yaml:113`), which fails when it checks no
ADR.

## More Information

* [MADR](https://adr.github.io/madr/)
* [adrlint](https://github.com/Goldziher/adrlint)
* [org ADR-0000 (Record architecture decisions)](https://github.com/xberg-io/agent-conventions/blob/main/adrs/0000-record-architecture-decisions.md)
