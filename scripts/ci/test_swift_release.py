"""Prove no release workflow moves a release ref and Swift is prepared by the shared workflow."""

from __future__ import annotations

import re
import unittest
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parents[2]
WORKFLOWS = ROOT / ".github/workflows"
REUSABLE = "xberg-io/actions/.github/workflows/reusable-swift-release-prepare.yml@v1"


def workflow(name: str) -> dict[str, object]:
    return yaml.safe_load((WORKFLOWS / name).read_text())


def steps(job: dict[str, object]) -> list[dict[str, object]]:
    return list(job.get("steps", []))


def uses(job: dict[str, object], action: str) -> list[dict[str, object]]:
    return [step for step in steps(job) if str(step.get("uses", "")).startswith(f"{action}@")]


MOVES_A_RELEASE_REF = re.compile(
    r"git\s+tag\s+(-\S*\s+)*-f|git\s+push[^\n]*(--force\b|\s\+[^\s]*refs/)|__ALEF_SWIFT_CHECKSUM__"
)


# ~keep Deletes and re-creates the release tag on `republish`; immutable releases never call it.
RETAG_ACTION = "xberg-io/actions/retag-for-republish@"


class ReleaseWorkflowTests(unittest.TestCase):
    def setUp(self) -> None:
        self.publish = workflow("publish.yaml")["jobs"]
        self.prepare = workflow("prepare-swift-release.yaml")["jobs"]

    def test_no_release_workflow_moves_a_tag_or_branch_or_rewrites_the_manifest(self) -> None:
        checked = 0
        for name, jobs in (("publish.yaml", self.publish), ("prepare-swift-release.yaml", self.prepare)):
            for job_name, job in jobs.items():
                for step in steps(job):
                    checked += 1
                    with self.subTest(workflow=name, job=job_name, step=step.get("name")):
                        assert not MOVES_A_RELEASE_REF.search(str(step.get("run", "")))
                        assert not str(step.get("uses", "")).startswith(RETAG_ACTION)
        assert checked > 200, checked
        assert MOVES_A_RELEASE_REF.search('git tag -f -a "$TAG" -m "$TAG" HEAD')
        assert MOVES_A_RELEASE_REF.search('git push origin "refs/tags/${TAG}" --force')

    def test_publish_never_builds_a_swift_bundle_and_reuses_the_prepared_one(self) -> None:
        assert not any(uses(job, "xberg-io/actions/build-swift-artifactbundle") for job in self.publish.values())
        job = self.publish["publish-swift-release"]
        (action,) = uses(job, "xberg-io/actions/publish-swift-release")
        assert action["uses"] == "xberg-io/actions/publish-swift-release@v1"
        assert action["with"]["bundle-zip"] == ""
        assert action["with"]["receipt"].endswith("Crawlberg-rs.artifactbundle.release.json")
        (checkout,) = uses(job, "actions/checkout")
        assert checkout["with"]["fetch-depth"] == 0
        assert "steps.app-token.outputs" in checkout["with"]["token"]

    def test_prepare_job_verifies_the_swift_release_before_any_publisher(self) -> None:
        (gate,) = uses(self.publish["prepare"], "xberg-io/actions/publish-swift-release")
        assert gate["with"]["dry-run"] == "true"
        assert "release_swift == 'true'" in gate["if"]

    def test_needs_name_existing_jobs_and_nothing_waits_on_a_retired_swift_job(self) -> None:
        retired = {"swift-artifactbundle", "upload-swift-release", "publish-swift-checksums"}
        for name, job in self.publish.items():
            needs = job.get("needs", [])
            needs = [needs] if isinstance(needs, str) else needs
            with self.subTest(job=name):
                assert set(needs) <= set(self.publish), set(needs) - set(self.publish)
                assert not retired & set(needs)
                assert not any(old in str(job.get("if", "")) for old in retired)
        for name in ("push-go-subdir-tag", "publish-homebrew-formula"):
            assert "publish-swift-release" not in self.publish[name]["needs"]
        for name in ("promote-release", "release-finalize", "release-report"):
            assert "publish-swift-release" in self.publish[name]["needs"]

    def test_preparation_is_the_shared_workflow_with_no_repo_local_build(self) -> None:
        assert list(self.prepare) == ["swift"]
        job = self.prepare["swift"]
        assert job["uses"] == REUSABLE
        assert "steps" not in job
        assert job["with"]["dry-run"] == "${{ inputs.dry-run }}"
        assert job["with"]["crate-name"] == "crawlberg-swift"
        assert job["with"]["artifact-name"] == "Crawlberg-rs"
        # alef's sync-versions accepts only a bundle whose info.json declares Package.swift's binary target.
        assert job["with"]["binary-target-name"] == "RustBridgeBinary"
        assert job["secrets"] == {
            "app-id": "${{ secrets.BOT_APP_ID }}",
            "app-private-key": "${{ secrets.BOT_APP_PRIVATE_KEY }}",
        }
        # ~keep publish.yaml reads the receipt the shared workflow attaches under this name.
        receipt = self.publish["publish-swift-release"]
        (action,) = uses(receipt, "xberg-io/actions/publish-swift-release")
        assert action["with"]["receipt"].endswith(f"{job['with']['artifact-name']}.artifactbundle.release.json")


if __name__ == "__main__":
    unittest.main()
