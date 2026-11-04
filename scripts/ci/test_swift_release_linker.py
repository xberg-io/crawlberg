"""Keep host-only extension linker flags out of the cross-platform Swift bundle."""

from __future__ import annotations

import unittest
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parents[2]


def caller() -> dict[str, object]:
    return yaml.safe_load((ROOT / ".github/workflows/prepare-swift-release.yaml").read_text())


class SwiftReleaseLinkerTests(unittest.TestCase):
    def test_bundle_installs_zig_017_compatible_linker_before_cross_compiling(self) -> None:
        workflow = caller()
        inputs = workflow["jobs"]["swift"]["with"]
        assert inputs["cargo-zigbuild-revision"] == "371bac84444fe6f998955889f2106802b817094b"
        publish = yaml.safe_load((ROOT / ".github/workflows/publish.yaml").read_text())
        assert "cargo-zigbuild" not in str(publish["jobs"])

    def test_bundle_opts_out_of_host_extension_linker_flags_before_cross_compiling(self) -> None:
        workflow = caller()
        job = workflow["jobs"]["swift"]
        assert job["with"]["macos-dynamic-lookup"] == "false"
        assert job["with"]["crate-name"] == "crawlberg-swift"
        for scope in (workflow, job):
            env = scope.get("env", {})
            assert "RUSTFLAGS" not in env
            assert "CARGO_ENCODED_RUSTFLAGS" not in env
            assert env.get("CARGO_BUILD_WARNINGS", "deny") == "deny"


if __name__ == "__main__":
    unittest.main()
