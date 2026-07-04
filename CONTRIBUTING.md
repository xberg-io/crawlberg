# Contributing

!!! note "Under Construction"
This page is being written. Check back soon.

## Pre-commit hooks

Install the git hooks with `task setup` (or `poly hooks install` directly). On
every commit, poly runs lint, format, and file-safety checks plus `cargo clippy`;
the commit-msg hook validates the message. Run all hooks manually with
`poly hooks run pre-commit --all-files`.
