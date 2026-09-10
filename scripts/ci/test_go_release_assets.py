"""Verify checksums survive the Go native release upload pipeline."""

from __future__ import annotations

import hashlib
import subprocess
import tempfile
import unittest
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parents[2]


class GoReleaseAssetsTests(unittest.TestCase):
    def test_generated_setup_receives_archive_and_verified_sidecar(self) -> None:
        workflow = yaml.safe_load((ROOT / ".github/workflows/publish.yaml").read_text())
        steps = workflow["jobs"]["go-ffi-libraries"]["steps"]
        checksum = next(step for step in steps if step.get("name") == "Write native archive checksum")
        upload = next(step for step in steps if "actions/upload-artifact@" in step.get("uses", ""))
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive = root / "crawlberg-go-linux-x86_64.tar.gz"
            archive.write_bytes(b"native archive payload")
            result = subprocess.run(
                ["bash", "-c", checksum["run"]], cwd=root, capture_output=True, text=True, check=False
            )
            assert result.returncode == 0, result.stderr
            sidecar = Path(f"{archive}.sha256")
            assert sidecar.read_text() == f"{hashlib.sha256(archive.read_bytes()).hexdigest()}  {archive.name}\n"
            pattern = upload["with"]["path"].removeprefix("dist/go-ffi/")
            assert sorted(path.name for path in root.glob(pattern)) == [archive.name, sidecar.name]
        release_steps = workflow["jobs"]["upload-go-release"]["steps"]
        release = next(step for step in release_steps if "upload-release-assets@" in step.get("uses", ""))
        assert release["with"]["assets"].split() == ["crawlberg-go-*.tar.gz", "crawlberg-go-*.tar.gz.sha256"]


if __name__ == "__main__":
    unittest.main()
