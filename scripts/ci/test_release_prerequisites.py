"""Exercise the real release gate and PHP packaging prerequisites."""

from __future__ import annotations

import os
import re
import subprocess
import tempfile
import unittest
from pathlib import Path

import yaml
from test_release_dry_run import release_condition

ROOT = Path(__file__).resolve().parents[2]


def run_gate(source: str, overrides: dict[str, str]) -> dict[str, str]:
    values = {
        "RESULT_PUBLISH_CRATES": "skipped",
        "RELEASE_CRATES": "true",
        "IS_TAG": "true",
        "DRY_RUN": "false",
        "FORCE_REPUBLISH": "false",
        "ALREADY_ON_CRATESIO": "false",
    }
    with tempfile.TemporaryDirectory() as directory:
        output = Path(directory) / "output"
        result = subprocess.run(
            ["bash", "-e", "-o", "pipefail", "-c", source],
            cwd=ROOT,
            env={**os.environ, **values, **overrides, "GITHUB_OUTPUT": str(output)},
            capture_output=True,
            text=True,
            check=False,
        )
        assert result.returncode == 0, result.stderr
        return dict(line.split("=", 1) for line in output.read_text().splitlines())


class ReleasePrerequisiteTests(unittest.TestCase):
    def setUp(self) -> None:
        self.jobs = yaml.safe_load((ROOT / ".github/workflows/publish.yaml").read_text())["jobs"]
        decisions = [step for step in self.jobs["crates-gate"]["steps"] if step.get("id") == "decide"]
        assert len(decisions) == 1, "Expected exactly one executable registry gate"
        self.gate = decisions[0]["run"]

    def test_registry_gate_distinguishes_simulation_from_published_packages(self) -> None:
        cases = (
            ({"RESULT_PUBLISH_CRATES": "success"}, "published", "true"),
            ({"RESULT_PUBLISH_CRATES": "failure"}, "blocked", "false"),
            ({"RESULT_PUBLISH_CRATES": "cancelled"}, "blocked", "false"),
            ({}, "blocked", "false"),
            ({"RELEASE_CRATES": "false"}, "not-required", "true"),
            ({"RELEASE_CRATES": "false", "DRY_RUN": "true"}, "not-required", "true"),
            ({"IS_TAG": "false"}, "not-a-tag", "false"),
            ({"ALREADY_ON_CRATESIO": "true"}, "already-present", "true"),
            ({"ALREADY_ON_CRATESIO": "true", "FORCE_REPUBLISH": "true"}, "blocked", "false"),
            ({"DRY_RUN": "true"}, "dry-run", "false"),
            ({"DRY_RUN": "true", "RESULT_PUBLISH_CRATES": "success"}, "dry-run", "false"),
            ({"DRY_RUN": "true", "ALREADY_ON_CRATESIO": "true"}, "already-present", "true"),
            (
                {"DRY_RUN": "true", "ALREADY_ON_CRATESIO": "true", "RESULT_PUBLISH_CRATES": "success"},
                "already-present",
                "true",
            ),
            (
                {"DRY_RUN": "true", "ALREADY_ON_CRATESIO": "true", "FORCE_REPUBLISH": "true"},
                "already-present",
                "true",
            ),
            ({"DRY_RUN": "true", "RESULT_PUBLISH_CRATES": "failure"}, "blocked", "false"),
            ({"DRY_RUN": "true", "RESULT_PUBLISH_CRATES": "cancelled"}, "blocked", "false"),
            ({"RESULT_PUBLISH_CRATES": "failure", "ALREADY_ON_CRATESIO": "true"}, "blocked", "false"),
            ({"RESULT_PUBLISH_CRATES": "cancelled", "ALREADY_ON_CRATESIO": "true"}, "blocked", "false"),
        )
        assert len(cases) == 18
        observed = 0
        for inputs, outcome, ready in cases:
            with self.subTest(inputs=inputs):
                observed += 1
                assert run_gate(self.gate, inputs) == {"outcome": outcome, "ok": ready}
        assert observed == 18
        print(f"Registry gate cases: expected=18 observed={observed}")

    def test_gate_negative_control_detects_simulated_publication(self) -> None:
        source = self.gate + '\necho "ok=true" >> "$GITHUB_OUTPUT"\n'
        rejected = False
        try:
            assert run_gate(source, {"DRY_RUN": "true", "RESULT_PUBLISH_CRATES": "success"})["ok"] == "false"
        except AssertionError:
            rejected = True
        assert rejected, "A simulated publication that emits ok=true must fail the readiness check"

    def test_registry_dependent_builds_require_the_gate_output(self) -> None:
        expected = {"python-sdist", "ruby-gem", "php-extension", "elixir-natives", "elixir-natives-musl"}
        builds = {name for name, job in self.jobs.items() if "crates-gate" in job.get("needs", []) and name in expected}
        assert builds == expected
        values = {
            "needs.prepare.outputs.force_republish": "false",
            **{f"needs.prepare.outputs.release_{target}": "true" for target in ("python", "ruby", "php", "elixir")},
            **{
                f"needs.{check}.outputs.exists": "false"
                for check in ("check-pypi", "check-rubygems", "check-packagist", "check-elixir-release")
            },
        }
        for name in sorted(builds):
            for ready in (False, True):
                with self.subTest(job=name, ready=ready):
                    values["needs.crates-gate.outputs.ok"] = str(ready).lower()
                    assert release_condition(self.jobs[name]["if"], values) is ready

    def test_php_native_build_runs_even_when_packagist_metadata_exists(self) -> None:
        values = {
            "needs.prepare.outputs.force_republish": "false",
            "needs.prepare.outputs.release_php": "true",
            "needs.crates-gate.outputs.ok": "true",
            "needs.check-packagist.outputs.exists": "true",
        }
        assert release_condition(self.jobs["php-extension"]["if"], values) is True

    def test_release_report_rejects_missing_php_assets_despite_packagist_metadata(self) -> None:
        reports = [
            step
            for job in self.jobs.values()
            for step in job.get("steps", [])
            if step.get("name") == "Verify every enabled publish target succeeded"
        ]
        assert len(reports) == 1
        report = reports[0]
        values = dict.fromkeys(report["env"], "false")
        values.update({name: "success" for name in values if name.startswith("RESULT_")})
        values.update(
            TAG="v1.12.0",
            CRATES_GATE_OK="true",
            RELEASE_PHP="true",
            EXISTS_PACKAGIST="true",
        )
        cases = (("skipped", 1), ("failure", 1), ("success", 0))
        observed = 0
        for result, code in cases:
            observed += 1
            with self.subTest(result=result):
                completed = subprocess.run(
                    ["bash", "-c", report["run"]],
                    env={**os.environ, **values, "RESULT_UPLOAD_PHP_PIE": result},
                    capture_output=True,
                    text=True,
                    check=False,
                )
                assert completed.returncode == code, completed.stdout + completed.stderr
                if code != 0:
                    assert "Publish target 'php-pie-assets'" in completed.stdout
        assert observed == 3
        print(f"PHP release report cases: expected=3 observed={observed}")

    def test_php_packager_installs_the_config_pinned_alef_first(self) -> None:
        steps = self.jobs["php-extension"]["steps"]
        installers = [step for step in steps if step.get("uses", "").startswith("xberg-io/actions/install-alef@")]
        packagers = [step for step in steps if step.get("uses", "").startswith("xberg-io/actions/package-php-pie@")]
        assert len(installers) == len(packagers) == 1
        installer, packager = installers[0], packagers[0]
        assert steps.index(installer) < steps.index(packager)
        assert re.fullmatch(r"xberg-io/actions/install-alef@[0-9a-f]{40}", installer["uses"])
        assert installer["with"]["version"] == "latest"
        assert installer["with"].get("allow-unreleased", "false") == "false"
        assert packager["with"]["dry-run"] == "${{ needs.prepare.outputs.dry_run }}"


if __name__ == "__main__":
    unittest.main()
