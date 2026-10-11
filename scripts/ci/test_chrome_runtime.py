"""Verify partial Chromium installs repair their content providers."""

from __future__ import annotations

import os
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/ci/prepare-chrome-runtime.sh"


class ChromeRuntimeTests(unittest.TestCase):
    def run_case(
        self,
        failures: int,
        connect_failure: bool = False,
        chromium_present: bool = True,
        legacy: bool = False,
    ) -> tuple[subprocess.CompletedProcess[str], list[str], subprocess.CompletedProcess[str]]:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            mock = root / "sudo"
            mock.write_text(r"""#!/usr/bin/env python3
import os, pathlib, sys
root = pathlib.Path(os.environ["CASE_ROOT"])
args = sys.argv[1:]
with (root / "calls").open("a") as log:
    log.write(" ".join(args) + "\n")
if args[:2] == ["snap", "install"]:
    count_file = root / args[2]
    count = int(count_file.read_text()) if count_file.exists() else 0
    count_file.write_text(str(count + 1))
    if args[2] != "chromium" and count < int(os.environ["FAILURES"]):
        sys.stderr.write("HTTP408 provider assertion unavailable\n")
        sys.exit(1)
    (root / (args[2] + ".installed")).touch()
if args[:3] == ["snap", "list", "chromium"] and os.environ["CHROMIUM_PRESENT"] == "0":
    sys.exit(1)
if args[:2] == ["snap", "connect"]:
    if os.environ["CONNECT_FAILURE"] == "1" or not (root / (args[3] + ".installed")).exists():
        sys.exit(1)
    (root / (args[3] + ".connected")).touch()
if args[0] == "chromium":
    ready = os.environ["CHROMIUM_PRESENT"] == "1" and all(
        (root / (provider + suffix)).exists()
        for provider in ["gnome-46-2404", "mesa-2404"]
        for suffix in [".installed", ".connected"]
    )
    if not ready:
        sys.stderr.write("Content snap command-chain not found: ensure slot is connected\n")
        sys.exit(1)
    sys.stdout.write("<html><head></head><body></body></html>\n")
""")
            mock.chmod(0o755)
            env = dict(
                os.environ,
                PATH=f"{root}:{os.environ['PATH']}",
                CASE_ROOT=str(root),
                FAILURES=str(failures),
                CONNECT_FAILURE=str(int(connect_failure)),
                CHROMIUM_PRESENT=str(int(chromium_present)),
            )
            script = SCRIPT
            if legacy:
                script = root / "legacy.sh"
                script.write_text("sudo snap install chromium --classic\n")
            result = subprocess.run(
                ["bash", str(script), "3", "0", "gnome-46-2404", "mesa-2404"],
                env=env,
                capture_output=True,
                text=True,
                check=False,
            )
            calls = (root / "calls").read_text().splitlines() if (root / "calls").exists() else []
            smoke = subprocess.run(
                [str(mock), "chromium", "--dump-dom", "about:blank"],
                env=env,
                capture_output=True,
                text=True,
                check=False,
            )
            return result, calls, smoke

    def test_installed_chromium_alone_should_not_make_runtime_ready(self) -> None:
        result, calls, smoke = self.run_case(0, legacy=True)
        assert result.returncode == 0
        assert calls == ["snap install chromium --classic"]
        assert smoke.returncode != 0
        assert "ensure slot is connected" in smoke.stderr

    def test_should_install_and_connect_providers_for_partial_chromium(self) -> None:
        result, calls, smoke = self.run_case(0)
        assert result.returncode == 0, result.stderr
        assert smoke.returncode == 0, smoke.stderr
        assert "<html>" in smoke.stdout
        assert calls == [
            "snap install gnome-46-2404",
            "snap install mesa-2404",
            "snap list chromium",
            "snap connect chromium:gnome-46-2404 gnome-46-2404",
            "snap connect chromium:gpu-2404 mesa-2404",
        ]

    def test_should_leave_new_chromium_install_to_shared_setup_action(self) -> None:
        result, calls, smoke = self.run_case(0, chromium_present=False)
        assert result.returncode == 0, result.stderr
        assert smoke.returncode != 0
        assert calls == ["snap install gnome-46-2404", "snap install mesa-2404", "snap list chromium"]

    def test_should_retry_each_provider_after_transient_assertion_failure(self) -> None:
        result, calls, smoke = self.run_case(1)
        assert result.returncode == 0, result.stderr
        assert smoke.returncode == 0, smoke.stderr
        assert calls.count("snap install gnome-46-2404") == 2
        assert calls.count("snap install mesa-2404") == 2
        assert calls[-1] == "snap connect chromium:gpu-2404 mesa-2404"

    def test_should_fail_when_provider_never_installs(self) -> None:
        result, calls, smoke = self.run_case(3)
        assert result.returncode != 0
        assert smoke.returncode != 0
        assert calls == ["snap install gnome-46-2404"] * 3
        assert "gnome-46-2404" in result.stderr

    def test_should_fail_when_content_connection_fails(self) -> None:
        result, calls, smoke = self.run_case(0, connect_failure=True)
        assert result.returncode != 0
        assert smoke.returncode != 0
        assert calls[-1] == "snap connect chromium:gnome-46-2404 gnome-46-2404"


if __name__ == "__main__":
    unittest.main()
