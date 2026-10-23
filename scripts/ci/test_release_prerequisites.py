"""Exercise the real release gate and PHP packaging prerequisites."""

from __future__ import annotations

import json
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


def assert_node_package_payload(payloads: dict[str, bytes]) -> set[str]:
    manifest = json.loads(payloads["package.json"])
    entrypoints = {manifest["main"], manifest["types"], *manifest["exports"]["."].values()}
    declared_files = {name for name in manifest["files"] if "*" not in name}
    required = {"package.json", *(name.removeprefix("./") for name in entrypoints), *declared_files}
    missing = sorted(required - payloads.keys())
    assert missing == [], f"Missing Node release files: {', '.join(missing)}"
    empty = sorted(name for name in required if len(payloads[name]) == 0)
    assert empty == [], f"Empty Node release files: {', '.join(empty)}"
    return required


class ReleasePrerequisiteTests(unittest.TestCase):
    def setUp(self) -> None:
        self.jobs = yaml.safe_load((ROOT / ".github/workflows/publish.yaml").read_text())["jobs"]
        decisions = [step for step in self.jobs["crates-gate"]["steps"] if step.get("id") == "decide"]
        assert len(decisions) == 1, "Expected exactly one executable registry gate"
        self.gate = decisions[0]["run"]

    def test_node_runtime_entrypoints_exist_in_a_clean_release_checkout(self) -> None:
        directory = "crates/crawlberg-node/"
        paths = subprocess.check_output(["git", "ls-files", "--", directory], cwd=ROOT, text=True).splitlines()
        payloads = {
            path.removeprefix(directory): subprocess.check_output(["git", "show", f":{path}"], cwd=ROOT)
            for path in paths
            if "/" not in path.removeprefix(directory)
        }
        required = assert_node_package_payload(payloads)
        assert required == {"package.json", "index.js", "binding.cjs", "index.mjs", "index.d.ts"}
        print(f"Node release runtime files: expected=5 observed={len(required)}")

    def test_node_package_rejects_a_missing_main_loader(self) -> None:
        directory = ROOT / "crates/crawlberg-node"
        names = {"package.json", "index.js", "binding.cjs", "index.mjs", "index.d.ts"}
        payloads = {name: (directory / name).read_bytes() for name in names}
        del payloads["index.js"]
        failure: AssertionError | None = None
        try:
            assert_node_package_payload(payloads)
        except AssertionError as error:
            failure = error
        assert failure is not None, "a package missing its main loader must be rejected"
        assert str(failure) == "Missing Node release files: index.js"

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

    def test_release_completion_rejects_cancelled_publishers_and_unfinished_finalization(self) -> None:
        jobs = ("release-finalize", "announce-discord")
        publishers = ("publish-rubygems", "publish-hex", "upload-php-pie-release")
        cases = (("success", True), ("skipped", True), ("failure", False), ("cancelled", False))
        observed = 0
        for name in jobs:
            prerequisites = publishers + (("release-finalize",) if name == "announce-discord" else ())
            for publisher in prerequisites:
                assert publisher in self.jobs[name]["needs"]
                for result, expected in cases:
                    if publisher == "release-finalize":
                        expected = result == "success"
                    results = dict.fromkeys(self.jobs[name]["needs"], "success")
                    results[publisher] = result
                    expression = self.jobs[name]["if"].replace("always()", "true")
                    expression = re.sub(
                        r"contains\(needs\.\*\.result, '([^']+)'\)",
                        lambda match, states=tuple(results.values()): str(match[1] in states).lower(),
                        expression,
                    )
                    values = {
                        "needs.prepare.outputs.is_tag": "true",
                        "needs.prepare.outputs.dry_run": "false",
                        "needs.prepare.outputs.is_prerelease": "false",
                        "needs.crates-gate.outputs.ok": "true",
                        **{f"needs.{job}.result": state for job, state in results.items()},
                    }
                    with self.subTest(job=name, publisher=publisher, result=result):
                        observed += 1
                        assert release_condition(expression, values) is expected
        assert observed == 28
        print(f"Release completion cases: expected=28 observed={observed}")

    def test_swift_archive_deployment_target_matches_the_public_macos_minimum(self) -> None:
        manifest = (ROOT / "Package.swift").read_text()
        minimums = re.findall(r"\.macOS\(\.v([0-9]+)(?:_([0-9]+))?\)", manifest)
        assert len(minimums) == 1, "Expected one public macOS deployment minimum"
        major, minor = minimums[0]
        expected = f"{major}.{minor or '0'}"
        builders = [
            step
            for step in self.jobs["swift-artifactbundle"]["steps"]
            if step.get("uses", "").startswith("xberg-io/actions/build-swift-artifactbundle@")
        ]
        assert len(builders) == 1, "Expected one native Swift artifact builder"
        assert builders[0].get("env", {}).get("MACOSX_DEPLOYMENT_TARGET") == expected

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
