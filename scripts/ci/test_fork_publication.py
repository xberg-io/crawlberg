"""Require the independently versioned browser fork before core crate publication."""

from __future__ import annotations

import importlib.util
import io
import json
import os
import subprocess
import tempfile
import unittest
import urllib.error
import urllib.response
from pathlib import Path
from typing import TYPE_CHECKING
from unittest.mock import patch

import yaml

if TYPE_CHECKING:
    from types import ModuleType
from test_plugin_dispatch import evaluate_condition

ROOT = Path(__file__).resolve().parents[2]
FORK_MANIFEST = "vendor/crawlberg-chromiumoxide/Cargo.toml"
FORK_NAME = "crawlberg-chromiumoxide"


def git(directory: Path, *arguments: str) -> str:
    result = subprocess.run(
        ["git", "-c", "core.hooksPath=/dev/null", "-c", "commit.gpgsign=false", "-C", str(directory), *arguments],
        capture_output=True,
        text=True,
        check=True,
    )
    return result.stdout.strip()


def release_fixture(directory: Path) -> tuple[Path, str, str]:
    origin = directory / "origin"
    origin.mkdir()
    git(origin, "-c", "init.templateDir=", "init", "--quiet", "--initial-branch=main")
    git(origin, "config", "user.name", "Test")
    git(origin, "config", "user.email", "test@example.invalid")
    (origin / "release.txt").write_text("immutable old release source\n")
    (origin / "Taskfile.yml").write_text("version: '3'\ntasks:\n  existing: {cmds: [echo existing]}\n")
    git(origin, "add", "release.txt", "Taskfile.yml")
    git(origin, "commit", "--quiet", "-m", "test: create old release")
    release_sha = git(origin, "rev-parse", "HEAD")
    for name in ("check_browser_fork.py", "fork-check.Taskfile.yml"):
        original = ROOT / "scripts/ci" / name
        if original.exists():
            destination = origin / "scripts/ci" / name
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_bytes(original.read_bytes())
    git(origin, "add", "scripts/ci")
    git(origin, "commit", "--quiet", "-m", "test: add workflow tooling")
    workflow_sha = git(origin, "rev-parse", "HEAD")
    checkout = directory / "release"
    subprocess.run(["git", "-c", "init.templateDir=", "clone", "--quiet", str(origin), str(checkout)], check=True)
    git(checkout, "switch", "--quiet", "--detach", release_sha)
    return checkout, release_sha, workflow_sha


def install_registry_boundary(directory: Path) -> Path:
    stub = directory / "http-boundary"
    stub.mkdir()
    (stub / "sitecustomize.py").write_text(
        "import io, json, os, urllib.error, urllib.request, urllib.response\n"
        "from pathlib import Path\n"
        "def urlopen(request, timeout):\n"
        "    expected = 'https://crates.io/api/v1/crates/crawlberg-chromiumoxide/0.9.4'\n"
        "    assert request.full_url == expected and timeout == 30\n"
        "    Path(os.environ['HTTP_AUDIT']).write_text(expected)\n"
        "    if os.environ['REGISTRY_EXISTS'] == 'false':\n"
        "        raise urllib.error.HTTPError(expected, 404, 'missing', None, None)\n"
        "    body = json.dumps({'version': {'crate': 'crawlberg-chromiumoxide', 'num': '0.9.4'}}).encode()\n"
        "    return urllib.response.addinfourl(io.BytesIO(body), {}, expected, 200)\n"
        "urllib.request.urlopen = urlopen\n"
    )
    return stub


def checkout_workflow_tools(release: Path, origin: Path, workflow_sha: str, checkout: dict) -> None:
    assert checkout["ref"] == "${{ github.workflow_sha }}"
    assert checkout["fetch-depth"] == 1 and checkout["persist-credentials"] is False
    assert checkout["sparse-checkout-cone-mode"] is False
    patterns = checkout["sparse-checkout"].split()
    assert patterns == ["/scripts/ci/check_browser_fork.py", "/scripts/ci/fork-check.Taskfile.yml"]
    tooling = release / checkout["path"]
    subprocess.run(
        [
            "git",
            "-c",
            "init.templateDir=",
            "clone",
            "--quiet",
            "--no-checkout",
            "--depth=1",
            origin.as_uri(),
            str(tooling),
        ],
        check=True,
    )
    git(tooling, "sparse-checkout", "set", "--no-cone", *patterns)
    git(tooling, "switch", "--quiet", "--detach", workflow_sha)
    assert git(tooling, "ls-files", "-t").count("H ") == 2
    assert not (tooling / "Taskfile.yml").exists()


class ForkPublicationTests(unittest.TestCase):
    def setUp(self) -> None:
        self.workflow = yaml.safe_load((ROOT / ".github/workflows/publish.yaml").read_text())
        self.steps = self.workflow["jobs"]["publish-crates"]["steps"]

    def registry_checker(self) -> ModuleType:
        path = ROOT / "scripts/ci/check_browser_fork.py"
        spec = importlib.util.spec_from_file_location("browser_fork_check", path)
        assert spec is not None and spec.loader is not None
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module

    def metadata_step(self) -> dict:
        matches = [step for step in self.steps if step.get("id") == "fork_metadata"]
        assert len(matches) == 1, "Expected one executable fork metadata step"
        return matches[0]

    def extract_metadata(self, manifest: str) -> tuple[subprocess.CompletedProcess[str], str]:
        step = self.metadata_step()
        assert step["shell"] == "bash"
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / FORK_MANIFEST
            source.parent.mkdir(parents=True)
            source.write_text(manifest)
            output = root / "output"
            result = subprocess.run(
                ["bash", "-e", "-o", "pipefail", "-c", step["run"]],
                cwd=root,
                env={**os.environ, "GITHUB_OUTPUT": str(output)},
                capture_output=True,
                text=True,
                check=False,
            )
            return result, output.read_text() if output.exists() else ""

    def test_fork_publishes_first_with_its_own_manifest_and_version(self) -> None:
        publishers = [
            step for step in self.steps if step.get("uses", "").startswith("xberg-io/actions/publish-crates@")
        ]
        assert len(publishers) == 2, "Publish the fork separately before the six release-version crates"
        fork, core = publishers
        assert fork["with"] == {
            "crates": FORK_NAME,
            "manifest-path": FORK_MANIFEST,
            "version": "${{ steps.fork_metadata.outputs.version }}",
            "dry-run": "${{ needs.prepare.outputs.dry_run }}",
        }
        assert core["with"] == {
            "crates": "${{ env.CRATES_TO_PUBLISH }}",
            "version": "${{ needs.prepare.outputs.version }}",
            "dry-run": "${{ needs.prepare.outputs.dry_run }}",
        }
        assert FORK_NAME not in self.workflow["env"]["CRATES_TO_PUBLISH"].split()
        assert len(self.workflow["env"]["CRATES_TO_PUBLISH"].split()) == 6
        assert self.steps.index(self.metadata_step()) < self.steps.index(fork) < self.steps.index(core)

    def test_metadata_reads_the_fork_version_instead_of_the_release_version(self) -> None:
        for version in ("0.9.3", "0.10.0-rc.1"):
            with self.subTest(version=version):
                result, output = self.extract_metadata(f'[package]\nname = "{FORK_NAME}"\nversion = "{version}"\n')
                assert result.returncode == 0, result.stderr
                assert output == f"version={version}\n"

    def test_missing_or_invalid_package_identity_cannot_emit_a_version(self) -> None:
        manifests = (
            '[package]\nversion = "0.9.3"\n',
            '[package]\nname = "other-crate"\nversion = "0.9.3"\n',
            f'[package]\nname = "{FORK_NAME}"\n',
            f'[package]\nname = "{FORK_NAME}"\nversion = ""\n',
            f'[package]\nname = "{FORK_NAME}"\nversion = "   "\n',
            f'[package]\nname = "{FORK_NAME}"\nversion = 93\n',
        )
        for manifest in manifests:
            with self.subTest(manifest=manifest):
                result, output = self.extract_metadata(manifest)
                assert result.returncode != 0
                assert output == ""

    def test_existing_fork_is_skipped_and_missing_fork_is_published(self) -> None:
        publisher = next(
            step for step in self.steps if step.get("name") == "Publish the browser fork before core crates"
        )
        assert "if" in publisher, "The existing fork must not be uploaded again unconditionally"
        for exists, expected in (("true", False), ("false", True), ("", False), ("unknown", False)):
            with self.subTest(exists=exists):
                condition = publisher["if"].replace("steps.fork_registry.outputs.exists", repr(exists))
                assert (
                    evaluate_condition(condition, event="release", plugins_only=False, ref="refs/tags/v1.12.0")
                    is expected
                )

    def test_registry_guard_is_executed_before_any_publisher_with_the_fork_version(self) -> None:
        guards = [step for step in self.steps if step.get("id") == "fork_registry"]
        assert len(guards) == 1
        guard = guards[0]
        assert guard["run"] == "task --taskfile fork-check.Taskfile.yml check"
        assert guard["working-directory"] == ".release-workflow/scripts/ci"
        assert guard["env"] == {"FORK_VERSION": "${{ steps.fork_metadata.outputs.version }}"}
        publisher = next(
            step for step in self.steps if step.get("name") == "Publish the browser fork before core crates"
        )
        assert self.steps.index(self.metadata_step()) < self.steps.index(guard) < self.steps.index(publisher)
        installers = [step for step in self.steps if step.get("uses", "").startswith("xberg-io/actions/setup-mise@")]
        assert len(installers) == 1 and installers[0]["uses"] == "xberg-io/actions/setup-mise@v1"
        assert "task" in installers[0]["with"]["install-args"].split()
        assert installers[0]["with"]["config-ref"] == "${{ github.sha }}"
        assert self.steps.index(installers[0]) < self.steps.index(guard)
        tools = [step for step in self.steps if step.get("name") == "Check out workflow publishing tools"]
        assert len(tools) == 1
        assert tools[0]["uses"] == "actions/checkout@v7"
        assert self.steps.index(self.metadata_step()) < self.steps.index(tools[0]) < self.steps.index(guard)
        tasks = yaml.safe_load((ROOT / ".task/tools/general.yml").read_text())["tasks"]
        assert tasks["ci:release:fork:check"]["cmds"] == ["python3 scripts/ci/check_browser_fork.py"]

    def test_registry_guard_works_when_the_release_has_no_workflow_tooling(self) -> None:
        guard = next(step for step in self.steps if step.get("id") == "fork_registry")
        tools = [step for step in self.steps if step.get("name") == "Check out workflow publishing tools"]
        with tempfile.TemporaryDirectory() as directory:
            temporary = Path(directory)
            release, release_sha, workflow_sha = release_fixture(temporary)
            stub = install_registry_boundary(temporary)
            if tools:
                assert len(tools) == 1
                checkout_workflow_tools(release, temporary / "origin", workflow_sha, tools[0]["with"])
            observed = 0
            for exists in ("true", "false"):
                output, audit = temporary / f"output-{exists}", temporary / f"http-{exists}"
                result = subprocess.run(
                    ["bash", "-e", "-o", "pipefail", "-c", guard["run"]],
                    cwd=release / guard.get("working-directory", "."),
                    capture_output=True,
                    text=True,
                    env={
                        **os.environ,
                        "FORK_VERSION": "0.9.4",
                        "GITHUB_OUTPUT": str(output),
                        "PYTHONPATH": str(stub),
                        "HTTP_AUDIT": str(audit),
                        "REGISTRY_EXISTS": exists,
                    },
                    check=False,
                )
                assert result.returncode == 0, result.stdout + result.stderr
                assert output.read_text() == f"exists={exists}\n"
                assert audit.read_text().endswith("/crawlberg-chromiumoxide/0.9.4")
                assert git(release, "rev-parse", "HEAD") == release_sha
                assert (release / "release.txt").read_text() == "immutable old release source\n"
                assert git(release, "diff", "--name-only", "HEAD") == ""
                assert not (release / "scripts/ci/check_browser_fork.py").exists()
                observed += 1
            assert observed == 2
            print(f"Old-release registry cases: expected=2 observed={observed}")

    def test_registry_guard_only_emits_booleans_for_verified_registry_responses(self) -> None:
        module = self.registry_checker()
        version = "0.9.4"
        url = f"https://crates.io/api/v1/crates/{FORK_NAME}/{version}"
        cases = (
            (json.dumps({"version": {"crate": FORK_NAME, "num": version}}).encode(), "true"),
            (urllib.error.HTTPError(url, 404, "not found", None, None), "false"),
            (urllib.error.HTTPError(url, 403, "forbidden", None, None), None),
            (urllib.error.HTTPError(url, 503, "unavailable", None, None), None),
            (urllib.error.URLError("offline"), None),
            (b"not JSON", None),
            (b"{}", None),
            (b'{"version": []}', None),
            (json.dumps({"version": {"crate": "other", "num": version}}).encode(), None),
            (json.dumps({"version": {"crate": FORK_NAME, "num": "0.9.3"}}).encode(), None),
        )
        assert len(cases) == 10
        for response, expected in cases:
            with self.subTest(response=response), tempfile.TemporaryDirectory() as directory:
                output = Path(directory) / "output"
                kwargs = (
                    {"side_effect": response}
                    if isinstance(response, Exception)
                    else {"return_value": urllib.response.addinfourl(io.BytesIO(response), {}, url, 200)}
                )
                with (
                    patch("urllib.request.urlopen", **kwargs) as request,
                    patch.dict(os.environ, {"FORK_VERSION": version, "GITHUB_OUTPUT": str(output)}),
                ):
                    if expected is None:
                        with self.assertRaises((ValueError, OSError)):  # noqa: PT027  # ~keep Stdlib-only CI.
                            module.main()
                        assert not output.exists()
                    else:
                        module.main()
                        assert output.read_text() == f"exists={expected}\n"
                    assert request.call_count == 1
                    assert request.call_args.args[0].full_url == url

    def test_registry_guard_rejects_unexpected_success_status(self) -> None:
        module = self.registry_checker()
        body = json.dumps({"version": {"crate": FORK_NAME, "num": "0.9.4"}}).encode()
        response = urllib.response.addinfourl(io.BytesIO(body), {}, "https://crates.io", 201)
        with patch("urllib.request.urlopen", return_value=response) as request:
            with self.assertRaisesRegex(  # noqa: PT027  # ~keep Stdlib-only CI.
                ValueError, "unexpected HTTP status 201"
            ):
                module.registry_exists("0.9.4")
            assert request.call_count == 1

    def test_registry_guard_rejects_missing_inputs_before_network_work(self) -> None:
        module = self.registry_checker()
        for version, output in (("", "/tmp/unused"), ("0.9.4", ""), ("   ", "/tmp/unused")):
            with (
                self.subTest(version=version, output=output),
                patch("urllib.request.urlopen") as request,
                patch.dict(os.environ, {"FORK_VERSION": version, "GITHUB_OUTPUT": output}),
            ):
                with self.assertRaisesRegex(  # noqa: PT027  # ~keep Stdlib-only CI.
                    ValueError, "must be nonempty"
                ):
                    module.main()
                assert request.call_count == 0


if __name__ == "__main__":
    unittest.main()
