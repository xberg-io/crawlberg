"""Prove the Swift release is pinned before tagging and that no workflow moves a release ref."""

from __future__ import annotations

import hashlib
import json
import os
import re
import subprocess
import tempfile
import unittest
from collections.abc import Callable
from pathlib import Path

import yaml
from swift_release import ReleaseError, finalize, verify, verify_manifest

ROOT = Path(__file__).resolve().parents[2]
WORKFLOWS = ROOT / ".github/workflows"
REPOSITORY = "xberg-io/crawlberg"
VERSION = "1.2.3"
TAG = f"v{VERSION}"
ARCHIVE = "Crawlberg-rs.artifactbundle.zip"
URL = f"https://github.com/{REPOSITORY}/releases/download/{TAG}/{ARCHIVE}"
SOURCE_MANIFEST = f"""let package = Package(
  targets: [
    .binaryTarget(
      name: "RustBridgeBinary",
      url: "{URL}",
      checksum: "__ALEF_SWIFT_CHECKSUM__"
    ),
  ]
)
"""
GIT_ENV = {
    "GIT_AUTHOR_NAME": "test",
    "GIT_AUTHOR_EMAIL": "test@example.invalid",
    "GIT_COMMITTER_NAME": "test",
    "GIT_COMMITTER_EMAIL": "test@example.invalid",
    "GIT_CONFIG_GLOBAL": os.devnull,
    "GIT_CONFIG_NOSYSTEM": "1",
}


def run(*args: str, cwd: Path) -> str:
    return subprocess.check_output(args, cwd=cwd, text=True, env={**os.environ, **GIT_ENV}).strip()


def raises_release_error(call: Callable[[], object]) -> bool:
    try:
        call()
    except ReleaseError:
        return True
    return False


def pinned(checksum: str) -> str:
    return SOURCE_MANIFEST.replace("__ALEF_SWIFT_CHECKSUM__", checksum)


class Release:
    """A source repository with a bare origin, a prepared bundle and its receipt."""

    def __init__(self, base: Path) -> None:
        self.origin = base / "origin.git"
        self.repo = base / "work"
        run("git", "init", "--quiet", "--bare", "--initial-branch=main", str(self.origin), cwd=base)
        run("git", "init", "--quiet", "--initial-branch=main", str(self.repo), cwd=base)
        run("git", "remote", "add", "origin", str(self.origin), cwd=self.repo)
        (self.repo / ".gitignore").write_text("dist/\n")
        (self.repo / "Package.swift").write_text(SOURCE_MANIFEST)
        run("git", "add", ".", cwd=self.repo)
        run("git", "commit", "--quiet", "-m", "release 1.2.3", cwd=self.repo)
        run("git", "push", "--quiet", "origin", "HEAD:refs/heads/main", cwd=self.repo)
        self.source = run("git", "rev-parse", "HEAD", cwd=self.repo)
        bundle = self.repo / "dist/swift-artifactbundle"
        bundle.mkdir(parents=True)
        self.zip = bundle / ARCHIVE
        self.zip.write_bytes(b"prepared bundle bytes")
        self.checksum = hashlib.sha256(self.zip.read_bytes()).hexdigest()
        self.receipt = bundle / "Crawlberg-rs.artifactbundle.release.json"
        self.receipt.write_text(
            json.dumps(
                {
                    "schema": 1,
                    "repository": REPOSITORY,
                    "source": self.source,
                    "version": VERSION,
                    "archive": ARCHIVE,
                    "sha256": self.checksum,
                    "triples": ["aarch64-apple-darwin"],
                    "artifact_version": "1.0.0",
                }
            )
        )
        (self.repo / "Package.swift").write_text(pinned(self.checksum))

    def remote(self, ref: str) -> str:
        return run("git", "rev-parse", ref, cwd=self.origin)

    def verify(self) -> str:
        return verify(self.repo, self.zip, self.receipt, VERSION, REPOSITORY)

    def fresh_checkout(self) -> None:
        """Return the work tree to a rerun's state: HEAD at the source, the pin uncommitted."""
        run("git", "reset", "--quiet", "--soft", self.source, cwd=self.repo)
        run("git", "reset", "--quiet", cwd=self.repo)
        if run("git", "tag", "--list", TAG, cwd=self.repo):
            run("git", "tag", "-d", TAG, cwd=self.repo)

    def finalize(self, dry_run: bool = False) -> dict[str, str]:
        previous = os.environ.copy()
        os.environ.update(GIT_ENV)
        try:
            return finalize(self.repo, TAG, "origin", "main", dry_run)
        finally:
            os.environ.clear()
            os.environ.update(previous)


class ManifestPinTests(unittest.TestCase):
    checksum = "ab" * 32

    def test_accepts_exactly_the_checksum_substitution(self) -> None:
        verify_manifest(SOURCE_MANIFEST, pinned(self.checksum), self.checksum, URL)
        verify_manifest(pinned("cd" * 32), pinned(self.checksum), self.checksum, URL)

    def test_rejects_placeholder_wrong_checksum_url_and_other_bytes(self) -> None:
        for manifest, checksum, url in (
            (SOURCE_MANIFEST, self.checksum, URL),
            (pinned("cd" * 32), self.checksum, URL),
            (pinned(self.checksum), self.checksum, URL.replace(TAG, "v1.2.2")),
            (pinned(self.checksum) + "// extra\n", self.checksum, URL),
            (pinned(self.checksum).replace("RustBridgeBinary", "Other"), self.checksum, URL),
            (pinned(self.checksum) + f'checksum: "{self.checksum}"\n', self.checksum, URL),
        ):
            with self.subTest(manifest=manifest[-40:], url=url):
                assert raises_release_error(lambda: verify_manifest(SOURCE_MANIFEST, manifest, checksum, url))


class FinalizeTests(unittest.TestCase):
    def setUp(self) -> None:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.release = Release(Path(directory.name))

    def test_verify_accepts_the_prepared_pin(self) -> None:
        assert self.release.verify() == self.release.checksum

    def test_verify_rejects_a_receipt_for_other_bytes_source_or_version(self) -> None:
        receipt = json.loads(self.release.receipt.read_text())
        for key, value in (("sha256", "cd" * 32), ("source", "0" * 40), ("version", "1.2.2"), ("archive", "x.zip")):
            with self.subTest(key=key):
                self.release.receipt.write_text(json.dumps({**receipt, key: value}))
                assert raises_release_error(self.release.verify)

    def test_verify_rejects_a_version_sync_that_changed_other_files(self) -> None:
        (self.release.repo / "Cargo.lock").write_text("drift\n")
        assert raises_release_error(self.release.verify)

    def test_first_run_tags_the_pin_commit_on_main(self) -> None:
        result = self.release.finalize()
        commit = result["commit"]
        assert result == {"commit": commit, "tag-created": "true", "branch-updated": "true"}
        assert self.release.remote("refs/heads/main") == commit
        assert self.release.remote(f"{TAG}^{{commit}}") == commit
        assert run("git", "cat-file", "-t", TAG, cwd=self.release.origin) == "tag"
        assert run("git", "rev-parse", f"{commit}^", cwd=self.release.origin) == self.release.source
        tagged = run("git", "show", f"{TAG}:Package.swift", cwd=self.release.origin)
        assert re.findall(r'checksum: "([0-9a-f]{64})"', tagged) == [self.release.checksum]
        assert "__ALEF_" not in tagged

    def test_retry_after_success_changes_no_ref(self) -> None:
        self.release.finalize()
        tag_object = self.release.remote(f"refs/tags/{TAG}")
        main = self.release.remote("refs/heads/main")
        self.release.fresh_checkout()
        result = self.release.finalize()
        assert result == {"commit": main, "tag-created": "false", "branch-updated": "false"}
        assert self.release.remote(f"refs/tags/{TAG}") == tag_object
        assert self.release.remote("refs/heads/main") == main

    def test_retry_after_the_pin_was_pushed_tags_that_commit(self) -> None:
        run("git", "add", "Package.swift", cwd=self.release.repo)
        run("git", "commit", "--quiet", "-m", "pin", cwd=self.release.repo)
        run("git", "push", "--quiet", "origin", "HEAD:refs/heads/main", cwd=self.release.repo)
        pushed = self.release.remote("refs/heads/main")
        self.release.fresh_checkout()
        result = self.release.finalize()
        assert result == {"commit": pushed, "tag-created": "true", "branch-updated": "false"}
        assert self.release.remote(f"{TAG}^{{commit}}") == pushed

    def test_main_that_moved_elsewhere_fails_before_any_tag(self) -> None:
        other = Path(self.release.repo.parent / "other")
        run("git", "clone", "--quiet", str(self.release.origin), str(other), cwd=self.release.repo.parent)
        (other / "README.md").write_text("unrelated\n")
        run("git", "add", "README.md", cwd=other)
        run("git", "commit", "--quiet", "-m", "unrelated", cwd=other)
        run("git", "push", "--quiet", "origin", "HEAD:refs/heads/main", cwd=other)
        moved = self.release.remote("refs/heads/main")
        assert raises_release_error(self.release.finalize)
        assert self.release.remote("refs/heads/main") == moved
        assert run("git", "tag", "--list", TAG, cwd=self.release.origin) == ""

    def test_existing_divergent_tag_fails_and_is_left_alone(self) -> None:
        run("git", "tag", "-a", TAG, "-m", TAG, self.release.source, cwd=self.release.repo)
        run("git", "push", "--quiet", "origin", f"refs/tags/{TAG}", cwd=self.release.repo)
        run("git", "tag", "-d", TAG, cwd=self.release.repo)
        tag_object = self.release.remote(f"refs/tags/{TAG}")
        assert raises_release_error(self.release.finalize)
        assert self.release.remote(f"refs/tags/{TAG}") == tag_object
        assert self.release.remote("refs/heads/main") == self.release.source

    def test_dry_run_pushes_nothing(self) -> None:
        self.release.finalize(dry_run=True)
        assert self.release.remote("refs/heads/main") == self.release.source
        assert run("git", "tag", "--list", TAG, cwd=self.release.origin) == ""


def workflow(name: str) -> dict[str, object]:
    return yaml.safe_load((WORKFLOWS / name).read_text())


def steps(job: dict[str, object]) -> list[dict[str, object]]:
    return list(job.get("steps", []))


def uses(job: dict[str, object], action: str) -> list[dict[str, object]]:
    return [step for step in steps(job) if str(step.get("uses", "")).startswith(f"{action}@")]


MOVES_A_RELEASE_REF = re.compile(
    r"git\s+tag\s+(-\S*\s+)*-f|git\s+push[^\n]*(--force\b|\s\+[^\s]*refs/)|__ALEF_SWIFT_CHECKSUM__"
)


class ReleaseWorkflowTests(unittest.TestCase):
    def setUp(self) -> None:
        self.publish = workflow("publish.yaml")["jobs"]
        self.prepare = workflow("release-swift-prepare.yaml")["jobs"]

    def test_no_release_workflow_moves_a_tag_or_branch_or_rewrites_the_manifest(self) -> None:
        checked = 0
        for name, jobs in (("publish.yaml", self.publish), ("release-swift-prepare.yaml", self.prepare)):
            for job_name, job in jobs.items():
                for step in steps(job):
                    checked += 1
                    with self.subTest(workflow=name, job=job_name, step=step.get("name")):
                        assert not MOVES_A_RELEASE_REF.search(str(step.get("run", "")))
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

    def test_preparation_pins_the_receipt_bound_bundle_before_tagging(self) -> None:
        build = self.prepare["swift-artifactbundle"]
        (bundle,) = uses(build, "xberg-io/actions/build-swift-artifactbundle")
        assert bundle["with"]["receipt-version"] == "${{ steps.version.outputs.version }}"
        assert "package-manifest-path" not in bundle["with"]
        runs = [str(step.get("run", "")) for step in steps(build)]
        (sync,) = [index for index, text in enumerate(runs) if "alef sync-versions --regen" in text]
        assert "git restore --source=HEAD" in runs[sync]
        assert "swift_release.py verify" in runs[sync + 1]
        tag = self.prepare["tag-release"]
        names = [str(step.get("run", "")) + str(step.get("uses", "")) for step in steps(tag)]
        order = [
            next(index for index, text in enumerate(names) if marker in text)
            for marker in (
                "download-artifact",
                "swift_release.py verify",
                "swift_release.py finalize",
                "publish-github-release",
                "publish-swift-release",
            )
        ]
        assert order == sorted(order)
        assert uses(tag, "actions/download-artifact")[0]["with"]["name"] == "swift-release-prepared"
        assert not uses(tag, "xberg-io/actions/build-swift-artifactbundle")


if __name__ == "__main__":
    unittest.main()
