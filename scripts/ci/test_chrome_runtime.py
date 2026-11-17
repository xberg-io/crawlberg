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
        *,
        chromium_failures: int = 0,
        connect_failure: bool = False,
        chromium_present: bool = True,
        attempts: str = "3",
        delay: str = "0",
        plugs: tuple[str, str] = ("gnome-46-2404", "gpu-2404"),
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
    failure_limit = os.environ["CHROMIUM_FAILURES"] if args[2] == "chromium" else os.environ["FAILURES"]
    if count < int(failure_limit):
        sys.stderr.write("HTTP408 provider assertion unavailable\n")
        sys.exit(1)
    (root / (args[2] + ".installed")).touch()
if args[:3] == ["snap", "connections", "--all"] and len(args) > 3:
    sys.stderr.write("error: cannot use --all with snap name\n")
    sys.exit(1)
if args == ["snap", "connections", "chromium"]:
    print("Interface  Plug  Slot  Notes")
    for index, plug in enumerate(os.environ["PLUGS"].split()):
        interface = f"content[{plug}]" if index == 0 else "content"
        slot = f"{plug}:{plug}" if index == 0 else "-"
        print(f"{interface}  chromium:{plug}  {slot}  -")
if args[:2] == ["snap", "connect"]:
    plug = args[2].split(":", 1)[1]
    if plug not in os.environ["PLUGS"].split():
        sys.stderr.write(f'snap chromium has no plug named {plug}\n')
        sys.exit(1)
    if os.environ["CONNECT_FAILURE"] == "1" or not (root / (args[3] + ".installed")).exists():
        sys.exit(1)
    (root / (args[3] + ".connected")).touch()
if args[0] == "chromium":
    providers = {
        "gnome-46-2404": "gnome-46-2404",
        "gpu-2404": "mesa-2404",
        "gnome-core26": "gnome-core26",
        "gpu-2604": "mesa-2604",
    }
    plugs = os.environ["PLUGS"].split()
    ready = len(plugs) == 2 and (root / "chromium.installed").exists() and all(
        (root / (provider + suffix)).exists()
        for provider in (providers.get(plug, plug) for plug in plugs)
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
                CHROMIUM_FAILURES=str(chromium_failures),
                CONNECT_FAILURE=str(int(connect_failure)),
                PLUGS=" ".join(plugs),
            )
            (root / "sleep").write_text(
                "#!/usr/bin/env python3\nimport os, pathlib, sys\n"
                'with (pathlib.Path(os.environ["CASE_ROOT"]) / "calls").open("a") as log:\n'
                '    log.write("sleep " + sys.argv[1] + "\\n")\n'
            )
            (root / "sleep").chmod(0o755)
            if chromium_present:
                (root / "chromium.installed").touch()
            result = subprocess.run(
                ["bash", str(SCRIPT), attempts, delay],
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

    def test_should_install_and_connect_providers_for_partial_chromium(self) -> None:
        result, calls, smoke = self.run_case(0)
        assert result.returncode == 0, result.stderr
        assert smoke.returncode == 0, smoke.stderr
        assert "<html>" in smoke.stdout
        assert calls == [
            "snap install chromium",
            "snap connections chromium",
            "snap install gnome-46-2404",
            "snap install mesa-2404",
            "snap connect chromium:gnome-46-2404 gnome-46-2404",
            "snap connect chromium:gpu-2404 mesa-2404",
        ]

    def test_should_connect_providers_for_core26_chromium(self) -> None:
        result, calls, smoke = self.run_case(0, plugs=("gnome-core26", "gpu-2604"))
        assert result.returncode == 0, result.stderr
        assert smoke.returncode == 0, smoke.stderr
        assert calls == [
            "snap install chromium",
            "snap connections chromium",
            "snap install gnome-core26",
            "snap install mesa-2604",
            "snap connect chromium:gnome-core26 gnome-core26",
            "snap connect chromium:gpu-2604 mesa-2604",
        ]

    def test_should_fail_without_a_recognized_pair_of_content_plugs(self) -> None:
        result, calls, _ = self.run_case(0, plugs=("gnome-core26", "gpu-2404"))
        assert result.returncode != 0
        assert "Unsupported Chromium content plugs" in result.stderr
        assert calls == ["snap install chromium", "snap connections chromium"]

    def test_should_connect_runtime_on_a_fresh_chromium_install(self) -> None:
        result, calls, smoke = self.run_case(0, chromium_present=False)
        assert result.returncode == 0, result.stderr
        assert smoke.returncode == 0, smoke.stderr
        assert calls[-2:] == [
            "snap connect chromium:gnome-46-2404 gnome-46-2404",
            "snap connect chromium:gpu-2404 mesa-2404",
        ]

    def test_should_retry_each_provider_after_transient_assertion_failure(self) -> None:
        result, calls, smoke = self.run_case(2, delay="7")
        assert result.returncode == 0, result.stderr
        assert smoke.returncode == 0, smoke.stderr
        assert calls.count("snap install gnome-46-2404") == 3
        assert calls.count("snap install mesa-2404") == 3
        assert calls.count("sleep 7") == 4
        assert calls[-1] == "snap connect chromium:gpu-2404 mesa-2404"

    def test_should_retry_chromium_after_a_transient_install_failure(self) -> None:
        result, calls, smoke = self.run_case(0, chromium_failures=1, chromium_present=False)
        assert result.returncode == 0, result.stderr
        assert calls.count("snap install chromium") == 2
        assert calls.count("sleep 0") == 1
        assert smoke.returncode == 0, smoke.stderr

    def test_should_fail_when_chromium_never_installs(self) -> None:
        result, calls, smoke = self.run_case(0, chromium_failures=3, chromium_present=False)
        assert result.returncode != 0
        assert calls.count("snap install chromium") == 3
        assert calls.count("sleep 0") == 2
        assert "provider chromium failed after 3 attempts" in result.stderr
        assert smoke.returncode != 0
        assert not any(call.startswith("snap connect") for call in calls)

    def test_should_fail_when_provider_never_installs(self) -> None:
        result, calls, smoke = self.run_case(3)
        assert result.returncode != 0
        assert smoke.returncode != 0
        assert calls.count("snap install gnome-46-2404") == 3
        assert calls.count("sleep 0") == 2
        assert "gnome-46-2404" in result.stderr

    def test_should_reject_invalid_retry_configuration_before_installing(self) -> None:
        for attempts, delay in [("0", "0"), ("-1", "0"), ("x", "0"), ("3", "-1"), ("3", "x")]:
            result, calls, _ = self.run_case(0, attempts=attempts, delay=delay)
            assert result.returncode != 0
            assert calls == []
            assert "retry configuration" in result.stderr

    def test_should_fail_when_content_connection_fails(self) -> None:
        result, calls, smoke = self.run_case(0, connect_failure=True)
        assert result.returncode != 0
        assert smoke.returncode != 0
        assert calls[-1] == "snap connect chromium:gnome-46-2404 gnome-46-2404"


if __name__ == "__main__":
    unittest.main()
