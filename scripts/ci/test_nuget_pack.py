"""Guard the release workflow's managed and native NuGet package closure."""

from __future__ import annotations

import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parents[2]
PACKAGE = "XbergIo.Crawlberg"
MANAGED = "Crawlberg.csproj"
RUNTIME = "../Crawlberg.Runtime/Crawlberg.Runtime.csproj"
OUTPUT = "../../../dist/nuget"


class NugetPackWorkflowTests(unittest.TestCase):
    def setUp(self) -> None:
        workflow = yaml.safe_load((ROOT / ".github/workflows/publish.yaml").read_text())
        self.steps = workflow["jobs"]["publish-nuget"]["steps"]
        self.pack = next(step for step in self.steps if step.get("name") == "Pack NuGet package")
        template = ROOT / "packages/csharp/Crawlberg/runtime.json.template"
        self.graph = json.loads(template.read_text().replace("{{VERSION}}", "1.6.0"))
        self.rids = sorted(rid for rid, entries in self.graph["runtimes"].items() if PACKAGE in entries)

    def run_pack(self, fail_rid: str = "") -> tuple[subprocess.CompletedProcess[str], list[list[str]]]:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "runtime.json").write_text(json.dumps(self.graph))
            calls = root / "calls.jsonl"
            dotnet = root / "dotnet"
            dotnet.write_text(
                "#!/usr/bin/env python3\n"
                "import json, os, sys\n"
                "with open(os.environ['PACK_CALLS'], 'a') as log:\n"
                "    log.write(json.dumps(sys.argv[1:]) + '\\n')\n"
                "if os.environ['FAIL_RID'] and '-p:PublishedRID=' + os.environ['FAIL_RID'] in sys.argv:\n"
                "    sys.exit(17)\n"
            )
            dotnet.chmod(0o755)
            environment = {
                **os.environ,
                "PATH": f"{root}:{os.environ['PATH']}",
                "PACK_CALLS": str(calls),
                "FAIL_RID": fail_rid,
            }
            result = subprocess.run(
                ["bash", "-e", "-o", "pipefail", "-c", self.pack["run"]],
                cwd=root,
                env=environment,
                capture_output=True,
                text=True,
                check=False,
            )
            recorded = [json.loads(line) for line in calls.read_text().splitlines()] if calls.exists() else []
            return result, recorded

    def test_pack_includes_every_concrete_runtime_before_managed_package(self) -> None:
        result, calls = self.run_pack()
        assert result.returncode == 0, result.stderr
        expected = [
            ["pack", RUNTIME, "-c", "Release", f"-p:PublishedRID={rid}", "--output", OUTPUT] for rid in self.rids
        ]
        expected.append(["pack", MANAGED, "-c", "Release", "--output", OUTPUT])
        assert calls == expected
        assert len(self.rids) == 6

    def test_runtime_pack_failure_stops_before_managed_package(self) -> None:
        result, calls = self.run_pack(self.rids[0])
        assert result.returncode == 17, result.stderr
        assert len(calls) == 1
        assert calls[0][1] == RUNTIME

    def test_registry_lookup_matches_managed_package_identity(self) -> None:
        workflow = yaml.safe_load((ROOT / ".github/workflows/publish.yaml").read_text())
        check = workflow["jobs"]["check-nuget"]["steps"][0]
        assert check["with"]["package"] == PACKAGE

    def test_managed_project_uses_supported_sdk_and_nested_staging(self) -> None:
        setup = next(step for step in self.steps if "actions/setup-dotnet@" in step.get("uses", ""))
        assert setup["with"]["dotnet-version"] == "10.0.x"
        stage = next(step for step in self.steps if step.get("name", "").startswith("Stage runtimes"))
        assert "packages/csharp/Crawlberg/runtimes" in stage["run"]


if __name__ == "__main__":
    unittest.main()
