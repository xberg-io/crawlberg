"""Keep host-only extension linker flags out of the cross-platform Swift bundle."""

from __future__ import annotations

import shlex
import unittest
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parents[2]


class SwiftReleaseLinkerTests(unittest.TestCase):
    def test_bundle_installs_zig_017_compatible_linker_before_cross_compiling(self) -> None:
        workflow = yaml.safe_load((ROOT / ".github/workflows/release-swift-prepare.yaml").read_text())
        job = workflow["jobs"]["swift-artifactbundle"]
        installers = [step for step in job["steps"] if "cargo install" in step.get("run", "")]
        assert len(installers) == 1
        installer = installers[0]
        assert shlex.split(installer["run"]) == [
            "cargo",
            "install",
            "--locked",
            "--force",
            "--git",
            "https://github.com/rust-cross/cargo-zigbuild",
            "--rev",
            "371bac84444fe6f998955889f2106802b817094b",
            "cargo-zigbuild",
        ]
        setup_index = next(
            index
            for index, step in enumerate(job["steps"])
            if step.get("uses", "").startswith("xberg-io/actions/setup-rust@")
        )
        bundle_index = next(
            index
            for index, step in enumerate(job["steps"])
            if step.get("uses", "").startswith("xberg-io/actions/build-swift-artifactbundle@")
        )
        assert setup_index < job["steps"].index(installer) < bundle_index
        assert installer.get("env", {}).get("CARGO_BUILD_WARNINGS", "deny") == "deny"
        assert all(
            "cargo-zigbuild" not in str(other)
            for name, other in workflow["jobs"].items()
            if name != "swift-artifactbundle"
        )

    def test_bundle_opts_out_of_host_extension_linker_flags_before_cross_compiling(self) -> None:
        workflow = yaml.safe_load((ROOT / ".github/workflows/release-swift-prepare.yaml").read_text())
        job = workflow["jobs"]["swift-artifactbundle"]
        setups = [step for step in job["steps"] if step.get("uses", "").startswith("xberg-io/actions/setup-rust@")]
        bundles = [
            step
            for step in job["steps"]
            if step.get("uses", "").startswith("xberg-io/actions/build-swift-artifactbundle@")
        ]
        assert len(setups) == len(bundles) == 1
        setup, bundle = setups[0], bundles[0]
        assert setup.get("with", {}).get("macos-dynamic-lookup") == "false"
        assert job["steps"].index(setup) < job["steps"].index(bundle)
        assert bundle["with"]["crate-name"] == "crawlberg-swift"
        for scope in (workflow, job, setup, bundle):
            env = scope.get("env", {})
            assert "RUSTFLAGS" not in env
            assert "CARGO_ENCODED_RUSTFLAGS" not in env
            assert env.get("CARGO_BUILD_WARNINGS", "deny") == "deny"


if __name__ == "__main__":
    unittest.main()
