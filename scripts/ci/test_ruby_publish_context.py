"""Keep artifact-only Ruby publication bound to the prepared source toolchain."""

from __future__ import annotations

import copy
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parents[2]


def assert_ruby_publish_context(steps: list[dict]) -> None:
    checkouts = [step for step in steps if step.get("uses", "").startswith("actions/checkout@")]
    assert len(checkouts) == 1, "Ruby publication requires exactly one source checkout"
    checkout = checkouts[0]
    assert checkout["uses"] == "actions/checkout@v7"
    assert checkout["with"]["ref"] == "${{ needs.prepare.outputs.source_sha }}", "Use the prepared release source"
    assert checkout["with"]["persist-credentials"] is False
    consumers = [
        step
        for step in steps
        if step.get("uses", "").startswith(("actions/download-artifact@", "xberg-io/actions/setup-mise@"))
    ]
    assert len(consumers) == 2, "Expected the gem download and mise setup"
    assert all(steps.index(checkout) < steps.index(step) for step in consumers), (
        "Checkout must preserve later artifacts"
    )
    setup = next(step for step in consumers if step["uses"].startswith("xberg-io/actions/setup-mise@"))
    assert setup["with"] == {"tools": "ruby"}, "Resolve the committed Ruby version and lock"


def assert_context_rejected(steps: list[dict], message: str) -> None:
    failure = None
    try:
        assert_ruby_publish_context(steps)
    except AssertionError as error:
        failure = str(error)
    assert failure is not None, "Invalid Ruby publication context was accepted"
    assert message in failure, failure


class RubyPublishContextTests(unittest.TestCase):
    def setUp(self) -> None:
        jobs = yaml.safe_load((ROOT / ".github/workflows/publish.yaml").read_text())["jobs"]
        self.steps = jobs["publish-rubygems"]["steps"]
        self.prepare = jobs["prepare"]

    def test_prepared_source_output_resolves_the_admitted_ref_instead_of_controller_head(self) -> None:
        assert self.prepare["outputs"]["source_sha"] == "${{ steps.source.outputs.sha }}"
        sources = [step for step in self.prepare["steps"] if step.get("id") == "source"]
        assert len(sources) == 1
        checkouts = [step for step in self.prepare["steps"] if step.get("uses", "").startswith("actions/checkout@")]
        assert len(checkouts) == 1
        assert checkouts[0]["with"]["fetch-depth"] == 0
        assert self.prepare["steps"].index(checkouts[0]) < self.prepare["steps"].index(sources[0])
        assert sources[0]["env"] == {"SOURCE_REF": "${{ steps.meta.outputs.checkout_ref }}"}
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "output"
            result = subprocess.run(
                ["bash", "-e", "-c", sources[0]["run"]],
                cwd=ROOT,
                env={**os.environ, "GITHUB_OUTPUT": str(output), "SOURCE_REF": "HEAD^"},
                capture_output=True,
                text=True,
                check=False,
            )
            assert result.returncode == 0, result.stderr
            expected = subprocess.check_output(["git", "rev-parse", "HEAD^{commit}^"], cwd=ROOT, text=True).strip()
            assert output.read_text() == f"sha={expected}\n"

    def test_prepared_source_resolution_rejects_a_missing_ref(self) -> None:
        sources = [step for step in self.prepare["steps"] if step.get("id") == "source"]
        assert len(sources) == 1
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "output"
            result = subprocess.run(
                ["bash", "-e", "-c", sources[0]["run"]],
                cwd=ROOT,
                env={**os.environ, "GITHUB_OUTPUT": str(output), "SOURCE_REF": "refs/tags/missing-source-fixture"},
                capture_output=True,
                text=True,
                check=False,
            )
            assert result.returncode != 0, "A missing release ref must not emit an empty source SHA"
            assert not output.exists()

    def test_publisher_loads_prepared_source_before_downloading_gems_and_selecting_ruby(self) -> None:
        assert_ruby_publish_context(self.steps)

    def test_context_check_rejects_the_original_artifact_only_job(self) -> None:
        steps = [step for step in self.steps if not step.get("uses", "").startswith("actions/checkout@")]
        assert_context_rejected(steps, "requires exactly one source checkout")

    def test_context_check_rejects_moving_source_and_artifact_deletion_order(self) -> None:
        for mutation, message in (("source", "prepared release source"), ("order", "preserve later artifacts")):
            with self.subTest(mutation=mutation):
                steps = copy.deepcopy(self.steps)
                checkouts = [step for step in steps if step.get("uses", "").startswith("actions/checkout@")]
                assert len(checkouts) == 1
                checkout = checkouts[0]
                if mutation == "source":
                    checkout["with"]["ref"] = "main"
                else:
                    steps.remove(checkout)
                    steps.append(checkout)
                assert_context_rejected(steps, message)


if __name__ == "__main__":
    unittest.main()
