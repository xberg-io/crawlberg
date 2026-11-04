"""Audit the root and plugin pnpm locks, failing on any advisory, audit error or empty audit."""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PROJECTS = (".", "plugin")
SHOWN_PATHS = 3


def describe(paths: list[str]) -> str:
    shown = ", ".join(paths[:SHOWN_PATHS])
    return f"{shown} (+{len(paths) - SHOWN_PATHS} more)" if len(paths) > SHOWN_PATHS else shown


def evaluate(project: str, audit: dict[str, object]) -> list[str]:
    """Return the errors in one project's `pnpm audit --json` report."""
    if "error" in audit:
        return [f"{project}: pnpm audit failed: {json.dumps(audit['error'])}"]
    metadata = audit.get("metadata")
    total = metadata.get("totalDependencies", 0) if isinstance(metadata, dict) else 0
    if not total:
        return [f"{project}: pnpm audit examined 0 dependencies"]

    errors: list[str] = []
    advisories = audit.get("advisories")
    for item in advisories.values() if isinstance(advisories, dict) else ():
        ghsa = item.get("github_advisory_id", "unknown")
        module = item.get("module_name", "unknown")
        paths = [path for finding in item.get("findings", []) for path in finding.get("paths", [])]
        errors.append(f"{project}: advisory {ghsa} in {module} via {describe(paths)}")
    return errors


def audit(project: str) -> dict[str, object]:
    # pnpm audit exits non-zero whenever it finds an advisory; the JSON report decides the outcome. ~keep
    result = subprocess.run(
        ["pnpm", "--dir", project, "audit", "--json"],  # noqa: S607
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=False,
    )
    try:
        return json.loads(result.stdout)
    except json.JSONDecodeError:
        return {"error": {"exit": result.returncode, "stderr": result.stderr.strip()[-500:]}}


def main() -> int:
    failed = False
    for project in PROJECTS:
        report = audit(project)
        errors = evaluate(project, report)
        for error in errors:
            print(f"::error::{error}")
        if not errors:
            print(f"{project}: no advisories in {report['metadata']['totalDependencies']} dependencies")
        failed = failed or bool(errors)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
