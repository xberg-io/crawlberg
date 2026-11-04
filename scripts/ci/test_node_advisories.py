"""Ensure the Node advisory gate fails on any advisory, an audit error and an empty audit."""

from __future__ import annotations

import importlib.util
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("check_node_advisories", ROOT / "scripts/ci/check_node_advisories.py")
assert SPEC is not None and SPEC.loader is not None
CHECK = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = CHECK
SPEC.loader.exec_module(CHECK)

GLOB = "GHSA-3g3m-2fjr-6pw4"
MINIMATCH = "GHSA-g3hv-75wg-m249"
UTIL_PATH = ".>@opencode/plugin>@opencode/util>@npmcli/arborist>@npmcli/package-json>glob"


def advisory(ghsa: str, module: str, *paths: str) -> dict[str, object]:
    return {"github_advisory_id": ghsa, "module_name": module, "findings": [{"version": "1.0.0", "paths": list(paths)}]}


def report(total: int, *advisories: dict[str, object]) -> dict[str, object]:
    return {
        "advisories": {str(index): item for index, item in enumerate(advisories)},
        "metadata": {"totalDependencies": total},
    }


class NodeAdvisoryTests(unittest.TestCase):
    def test_clean_audit_passes(self) -> None:
        assert CHECK.evaluate("plugin", report(15)) == []

    def test_each_advisory_fails(self) -> None:
        audit = report(277, advisory(GLOB, "glob", UTIL_PATH), advisory(MINIMATCH, "minimatch", f"{UTIL_PATH}>minimatch"))
        errors = CHECK.evaluate("plugin", audit)
        assert len(errors) == 2
        assert GLOB in errors[0]
        assert MINIMATCH in errors[1]

    def test_advisory_paths_are_truncated(self) -> None:
        paths = [f".>dep{index}>glob" for index in range(5)]
        errors = CHECK.evaluate(".", report(424, advisory(GLOB, "glob", *paths)))
        assert errors == [f".: advisory {GLOB} in glob via .>dep0>glob, .>dep1>glob, .>dep2>glob (+2 more)"]

    def test_audit_without_dependencies_fails(self) -> None:
        errors = CHECK.evaluate(".", report(0))
        assert errors == [".: pnpm audit examined 0 dependencies"]

    def test_audit_error_payload_fails(self) -> None:
        errors = CHECK.evaluate(".", {"error": {"code": "ERR_PNPM_AUDIT_BAD_RESPONSE"}})
        assert len(errors) == 1
        assert "ERR_PNPM_AUDIT_BAD_RESPONSE" in errors[0]


if __name__ == "__main__":
    unittest.main()
