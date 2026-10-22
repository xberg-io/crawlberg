"""Verify checksums survive the Go native release upload pipeline."""

from __future__ import annotations

import hashlib
import os
import re
import subprocess
import tempfile
import unittest
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parents[2]


class GoReleaseAssetsTests(unittest.TestCase):
    def test_generated_setup_receives_both_archive_names_and_verified_sidecars(self) -> None:
        workflow = yaml.safe_load((ROOT / ".github/workflows/publish.yaml").read_text())
        job = workflow["jobs"]["go-ffi-libraries"]
        checksum = next(step for step in job["steps"] if step.get("name") == "Write native archive checksum")
        upload = next(step for step in job["steps"] if "actions/upload-artifact@" in step.get("uses", ""))
        platforms = [entry["platform"] for entry in job["strategy"]["matrix"]["include"]]
        assert len(platforms) == 5
        assert checksum["env"] == {
            "RELEASE_VERSION": "${{ needs.prepare.outputs.version }}",
            "RELEASE_PLATFORM": "${{ matrix.platform }}",
        }
        setup = (ROOT / "packages/go/cmd/setup/main.go").read_text()
        version_match = re.search(r'moduleVersion\s*=\s*"([^"]+)"', setup)
        pattern_match = re.search(r'assetName := fmt.Sprintf\("([^"]+)"', setup)
        prefix_match = re.search(r'assetPrefix\s*=\s*"([^"]+)"', setup)
        assert version_match is not None and pattern_match is not None and prefix_match is not None
        version, installer_pattern, prefix = version_match.group(1), pattern_match.group(1), prefix_match.group(1)
        for platform in platforms:
            with self.subTest(platform=platform), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                original = root / f"crawlberg-go-{platform}.tar.gz"
                original.write_bytes(b"native archive payload")
                result = subprocess.run(
                    ["bash", "-c", checksum["run"]],
                    cwd=root,
                    capture_output=True,
                    text=True,
                    check=False,
                    env={**os.environ, "RELEASE_VERSION": version, "RELEASE_PLATFORM": platform},
                )
                assert result.returncode == 0, result.stderr
                versioned = root / (installer_pattern % (prefix, version, platform))
                assert versioned.is_file(), "The published setup command must find its exact archive filename"
                assert versioned.read_bytes() == original.read_bytes()
                expected = []
                for archive in (original, versioned):
                    sidecar = Path(f"{archive}.sha256")
                    assert (
                        sidecar.read_text() == f"{hashlib.sha256(archive.read_bytes()).hexdigest()}  {archive.name}\n"
                    )
                    expected.extend((archive.name, sidecar.name))
                pattern = upload["with"]["path"].removeprefix("dist/go-ffi/")
                assert sorted(path.name for path in root.glob(pattern)) == sorted(expected)
        release_steps = workflow["jobs"]["upload-go-release"]["steps"]
        release = next(step for step in release_steps if "upload-release-assets@" in step.get("uses", ""))
        assert release["with"]["assets"].split() == ["crawlberg-go-*.tar.gz", "crawlberg-go-*.tar.gz.sha256"]

    def test_invalid_release_metadata_cannot_create_native_aliases(self) -> None:
        workflow = yaml.safe_load((ROOT / ".github/workflows/publish.yaml").read_text())
        steps = workflow["jobs"]["go-ffi-libraries"]["steps"]
        checksum = next(step for step in steps if step.get("name") == "Write native archive checksum")
        cases = (
            ("", "linux-x86_64", "version"),
            ("../1.12.0", "linux-x86_64", "version"),
            ("1.12.0", "", "platform"),
            ("1.12.0", "../linux", "platform"),
        )
        for version, platform, invalid_field in cases:
            with self.subTest(version=version, platform=platform), tempfile.TemporaryDirectory() as directory:
                original = Path(directory) / "crawlberg-go-linux-x86_64.tar.gz"
                original.write_bytes(b"native archive payload")
                result = subprocess.run(
                    ["bash", "-c", checksum["run"]],
                    cwd=directory,
                    capture_output=True,
                    text=True,
                    check=False,
                    env={**os.environ, "RELEASE_VERSION": version, "RELEASE_PLATFORM": platform},
                )
                assert result.returncode != 0
                assert f"Missing or invalid Go native release {invalid_field}" in result.stderr
                assert list(Path(directory).iterdir()) == [original]
                assert original.read_bytes() == b"native archive payload"

    def test_go_smoke_uses_the_published_setup_command(self) -> None:
        tasks = yaml.safe_load((ROOT / ".task/tools/test-apps.yml").read_text())["tasks"]
        commands = tasks["smoke:go"]["cmds"]
        module = (ROOT / "packages/go/go.mod").read_text().splitlines()[0].removeprefix("module ")
        assert f"go run {module}/cmd/setup" in commands
        assert (ROOT / "packages/go/cmd/setup/main.go").is_file()


if __name__ == "__main__":
    unittest.main()
