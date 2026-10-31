"""Keep the secure fetcher independent from Core and ahead of its driver publisher."""

from __future__ import annotations

import io
import json
import os
import subprocess
import tempfile
import tomllib
import unittest
import urllib.response
from pathlib import Path
from unittest.mock import patch

import yaml
from check_browser_fork import registry_exists

ROOT = Path(__file__).resolve().parents[2]
FETCHER = "crawlberg-chromiumoxide-fetcher"
DRIVER = "crawlberg-chromiumoxide"


class SecureFetcherPackagingTests(unittest.TestCase):
    def setUp(self) -> None:
        self.workflow = yaml.safe_load((ROOT / ".github/workflows/publish.yaml").read_text())
        self.steps = self.workflow["jobs"]["publish-crates"]["steps"]

    def test_core_keeps_the_registry_driver_and_its_existing_type_identity(self) -> None:
        root = tomllib.loads((ROOT / "Cargo.toml").read_text())
        dependency = root["workspace"]["dependencies"]["chromiumoxide"]
        assert dependency == {"package": DRIVER, "version": "0.9.5", "default-features": False}
        driver = tomllib.loads((ROOT / f"vendor/{DRIVER}/Cargo.toml").read_text())
        fetcher = tomllib.loads((ROOT / f"vendor/{FETCHER}/Cargo.toml").read_text())
        assert driver["package"]["version"] == "0.10.0"
        assert driver["package"]["rust-version"] == fetcher["package"]["rust-version"] == "1.88"
        assert driver["dependencies"]["chromiumoxide_fetcher"] == {
            "package": FETCHER,
            "version": "0.9.2",
            "path": f"../{FETCHER}",
            "optional": True,
            "default-features": False,
        }
        assert fetcher["package"]["version"] == "0.9.2"
        assert fetcher["features"]["zip0"] == fetcher["features"]["zip8"] == ["dep:zip"]
        assert fetcher["dependencies"]["zip"]["version"] == "9.2.0"

    def test_publishers_order_fetcher_then_driver_then_core_with_independent_versions(self) -> None:
        publishers = [s for s in self.steps if s.get("uses", "").startswith("xberg-io/actions/publish-crates@")]
        assert len(publishers) == 3, "Expected independently versioned fetcher, driver and Core publication"
        fetcher, driver, core = publishers
        assert fetcher["with"] == {
            "crates": FETCHER,
            "manifest-path": f"vendor/{FETCHER}/Cargo.toml",
            "version": "${{ steps.fetcher_metadata.outputs.version }}",
            "dry-run": "${{ needs.prepare.outputs.dry_run }}",
        }
        assert driver["with"]["crates"] == DRIVER
        assert driver["with"]["version"] == "${{ steps.fork_metadata.outputs.version }}"
        assert core["with"]["crates"] == "${{ env.CRATES_TO_PUBLISH }}"
        assert FETCHER not in self.workflow["env"]["CRATES_TO_PUBLISH"].split()
        guards = [step for step in self.steps if step.get("id") == "fetcher_registry"]
        assert len(guards) == 1
        assert guards[0]["env"] == {
            "FORK_PACKAGE": FETCHER,
            "FORK_VERSION": "${{ steps.fetcher_metadata.outputs.version }}",
        }
        assert guards[0]["if"] == "steps.fetcher_metadata.outputs.required == 'true'"
        assert self.steps.index(guards[0]) < self.steps.index(fetcher)
        assert fetcher["if"] == (
            "steps.fetcher_metadata.outputs.required == 'true' && steps.fetcher_registry.outputs.exists == 'false'"
        )

    def test_registry_checks_the_fetcher_identity_instead_of_the_driver(self) -> None:
        url = f"https://crates.io/api/v1/crates/{FETCHER}/0.9.2"
        body = json.dumps({"version": {"crate": FETCHER, "num": "0.9.2"}}).encode()
        response = urllib.response.addinfourl(io.BytesIO(body), {}, url, 200)
        with patch("urllib.request.urlopen", return_value=response) as request:
            assert registry_exists("0.9.2", FETCHER) is True
            assert request.call_count == 1
            assert request.call_args.args[0].full_url == url
        with patch("urllib.request.urlopen") as request:
            failure = None
            try:
                registry_exists("0.9.2", "unrelated-crate")
            except ValueError as error:
                failure = str(error)
            assert failure == "Unexpected browser package identity"
            assert request.call_count == 0

    def test_ci_declares_windows_and_msrv_checks_without_skipping_extraction(self) -> None:
        workflow = yaml.safe_load((ROOT / ".github/workflows/ci-fetcher-security.yaml").read_text())
        job = workflow["jobs"]["fetcher"]
        matrix = job["strategy"]["matrix"]["include"]
        assert len(matrix) == 3
        msrv_file = ".github/toolchains/fetcher-msrv/rust-toolchain.toml"
        assert {row["os"] for row in matrix} == {"ubuntu-latest", "windows-latest"}
        assert sum(row["toolchain_file"] == msrv_file for row in matrix) == 1
        assert tomllib.loads((ROOT / msrv_file).read_text())["toolchain"]["channel"] == "1.88.0"
        assert job["strategy"]["fail-fast"] is False
        assert job["env"]["CARGO_BUILD_JOBS"] == 2
        assert job["env"]["RUSTC_WRAPPER"] == ""
        tests = [step for step in job["steps"] if step.get("name", "").startswith("Test ")]
        assert len(tests) == 3
        assert all("--locked" in step["run"] for step in tests)
        guards = [
            step for step in job["steps"] if step.get("name") == "Require all five Windows extraction regressions"
        ]
        assert len(guards) == 1
        assert guards[0]["if"] == "runner.os == 'Windows'"
        assert 'test "$count" -eq 5' in guards[0]["run"]
        source = (ROOT / f"vendor/{FETCHER}/tests/windows_extraction.rs").read_text()
        assert source.count("#[tokio::test]") == 5
        assert "#[ignore" not in source

    def test_fetcher_metadata_preserves_old_release_sources_without_the_new_dependency(self) -> None:
        metadata = [s for s in self.steps if s.get("id") == "fetcher_metadata"]
        assert len(metadata) == 1
        for required in (False, True):
            with self.subTest(required=required), tempfile.TemporaryDirectory() as directory:
                source = Path(directory)
                driver = source / f"vendor/{DRIVER}/Cargo.toml"
                driver.parent.mkdir(parents=True)
                dependency = f'package = "{FETCHER}"\n' if required else ""
                driver.write_text('[dependencies.chromiumoxide_fetcher]\nversion = "0.9.2"\n' + dependency)
                if required:
                    fetcher = source / f"vendor/{FETCHER}/Cargo.toml"
                    fetcher.parent.mkdir(parents=True)
                    fetcher.write_text(f'[package]\nname = "{FETCHER}"\nversion = "0.9.2"\n')
                output = source / "output"
                result = subprocess.run(
                    ["bash", "-e", "-o", "pipefail", "-c", metadata[0]["run"]],
                    cwd=source,
                    env={**os.environ, "GITHUB_OUTPUT": str(output)},
                    capture_output=True,
                    text=True,
                    check=False,
                )
                assert result.returncode == 0, result.stderr
                expected = "required=true\nversion=0.9.2\n" if required else "required=false\n"
                assert output.read_text() == expected


if __name__ == "__main__":
    unittest.main()
