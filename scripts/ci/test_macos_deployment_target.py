"""Every shipped macOS binary targets the macOS minimum the root Package.swift declares."""

from __future__ import annotations

import re
import unittest
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parents[2]
SETTING = "MACOSX_DEPLOYMENT_TARGET"
# ~keep `VAR=value`, `VAR: value` and `VAR = "value"` spellings in run blocks, action inputs, shell and TOML.
ASSIGNMENT = re.compile(rf"\b{SETTING}[\"']?\s*[=:]\s*[\"']?(\$\{{[^}}]*\}}|[^\s\"',}}]+)")


def package_minimum() -> str:
    found = re.findall(r"\.macOS\(\.v(\d+)(?:_(\d+))?\)", (ROOT / "Package.swift").read_text())
    assert len(found) == 1, found
    major, minor = found[0]
    return f"{major}.{minor or '0'}"


def yaml_targets(document: object, where: str = "") -> list[tuple[str, str]]:
    found = []
    if isinstance(document, dict):
        for key, value in document.items():
            if key == SETTING:
                found.append((f"{where}/{key}", str(value)))
            found.extend(yaml_targets(value, f"{where}/{key}"))
    elif isinstance(document, list):
        for index, value in enumerate(document):
            found.extend(yaml_targets(value, f"{where}[{index}]"))
    elif isinstance(document, str):
        found.extend((where, value) for value in ASSIGNMENT.findall(document))
    return found


def text_targets(path: Path) -> list[tuple[str, str]]:
    return [(str(path.relative_to(ROOT)), value) for value in ASSIGNMENT.findall(path.read_text(errors="ignore"))]


def configured_targets() -> list[tuple[str, str]]:
    found = []
    for path in sorted((ROOT / ".github/workflows").glob("*.y*ml")):
        found.extend((f"{path.name}{where}", value) for where, value in yaml_targets(yaml.safe_load(path.read_text())))
    sources = [ROOT / "mise.toml", ROOT / ".cargo/config.toml"]
    sources += [path for path in (ROOT / "mise-tasks").rglob("*") if path.is_file()]
    # ~keep Test files hold deliberately wrong fixture values; the code they test is scanned here.
    sources += [path for path in (ROOT / "scripts").rglob("*") if path.is_file() and not path.name.startswith("test_")]
    for path in sources:
        if path.is_file() and path.suffix != ".pyc":
            found.extend(text_targets(path))
    return found


def macos_job(job: dict[str, object]) -> bool:
    runs_on = str(job.get("runs-on", ""))
    if "macos" in runs_on:
        return True
    return "matrix" in runs_on and "macos" in str(job.get("strategy", {}))


class MacosDeploymentTargetTests(unittest.TestCase):
    def test_package_minimum_is_macos_26(self) -> None:
        assert package_minimum() == "26.0"

    def test_every_configured_target_equals_the_package_minimum(self) -> None:
        found = configured_targets()
        assert len(found) >= 3, found
        assert [entry for entry in found if entry[1] != package_minimum()] == []

    def test_every_macos_publish_job_gets_the_target(self) -> None:
        workflow = yaml.safe_load((ROOT / ".github/workflows/publish.yaml").read_text())
        inherited = workflow.get("env", {}).get(SETTING)
        jobs = {name: job for name, job in workflow["jobs"].items() if macos_job(job)}
        assert len(jobs) >= 5, sorted(jobs)
        for name, job in jobs.items():
            with self.subTest(job=name):
                assert job.get("env", {}).get(SETTING, inherited) == package_minimum()

    def test_detectors_find_nested_and_inline_low_values(self) -> None:
        document = {
            "jobs": {
                "swift": {"steps": [{"env": {SETTING: "13.0"}}, {"run": f"{SETTING}=11.0 cargo build"}]},
                "wheels": {"with": {"cibw-environment-macos": f'A="" {SETTING}=10.12'}},
            }
        }
        assert sorted(value for _, value in yaml_targets(document)) == ["10.12", "11.0", "13.0"]
        assert ASSIGNMENT.findall(f'export {SETTING}="${{X:-11.0}}"') == ["${X:-11.0}"]
        assert ASSIGNMENT.findall(f'{SETTING} = "26.0"') == ["26.0"]
        assert macos_job({"runs-on": "${{ matrix.os }}", "strategy": {"matrix": {"os": ["macos-26"]}}})
        assert not macos_job({"runs-on": "${{ matrix.os }}", "strategy": {"matrix": {"os": ["ubuntu-latest"]}}})


if __name__ == "__main__":
    unittest.main()
