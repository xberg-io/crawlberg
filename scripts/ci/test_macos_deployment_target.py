"""Every shipped macOS binary targets the lowest macOS that the code and the toolchain need.

That is macOS 10.13 on x86_64 and macOS 11.0 on arm64: Apple Silicon has no macOS before 11, and clang, rustc and
maturin raise an arm64 target below 11 to 11.0. The `MACOSX_DEPLOYMENT_TARGET` of publish.yaml is the one source of
truth. The Swift package has its own minimum, which test_swift_macos_minimum.py guards; the shared Swift prepare
workflow derives the release bundle's target from Package.swift itself.
"""

from __future__ import annotations

import re
import unittest
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parents[2]
SETTING = "MACOSX_DEPLOYMENT_TARGET"
# ~keep The shared reusable workflows take the target as an input, not an env value.
CALLER_INPUT = "macos-deployment-target"
ARM64_MINIMUM = "11.0"
# ~keep `VAR=value`, `VAR: value` and `VAR = "value"` spellings in run blocks, action inputs, shell and TOML.
ASSIGNMENT = re.compile(rf"\b{SETTING}[\"']?\s*[=:]\s*[\"']?(\$\{{[^}}]*\}}|[^\s\"',}}]+)")


def deployment_target() -> str:
    return str(yaml.safe_load((ROOT / ".github/workflows/publish.yaml").read_text())["env"][SETTING])


def version(value: str) -> tuple[int, ...]:
    return tuple(map(int, value.split(".")))


def yaml_targets(document: object, where: str = "") -> list[tuple[str, str]]:
    found = []
    if isinstance(document, dict):
        for key, value in document.items():
            if key in {SETTING, CALLER_INPUT}:
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


# ~keep Reusable workflows that build shipped macOS binaries. Workflow env does not reach a called
# ~keep workflow, so the CLI caller must pass the target as the `macos-deployment-target` input. The
# ~keep Swift prepare workflow derives the bundle's target from Package.swift; an override there could
# ~keep only restate the Swift minimum or fail the run.
CLI_REUSABLE = "reusable-cli-release.yml"
SWIFT_REUSABLE = "reusable-swift-release-prepare.yml"
MACOS_BUILDING_REUSABLES = (CLI_REUSABLE, SWIFT_REUSABLE)


def reusable_macos_builds() -> dict[tuple[str, str], dict[str, object]]:
    found = {}
    for path in sorted((ROOT / ".github/workflows").glob("*.y*ml")):
        for name, job in yaml.safe_load(path.read_text())["jobs"].items():
            if any(f"/{reusable}@" in str(job.get("uses", "")) for reusable in MACOS_BUILDING_REUSABLES):
                found[(path.name, name)] = job
    return found


class MacosDeploymentTargetTests(unittest.TestCase):
    def test_the_deployment_target_is_10_13_and_below_the_arm64_minimum(self) -> None:
        assert deployment_target() == "10.13"
        assert version(deployment_target()) < version(ARM64_MINIMUM)

    def test_every_configured_target_equals_the_publish_target(self) -> None:
        found = configured_targets()
        assert len(found) >= 3, found
        assert [entry for entry in found if entry[1] != deployment_target()] == []

    def test_every_macos_publish_job_gets_the_target(self) -> None:
        workflow = yaml.safe_load((ROOT / ".github/workflows/publish.yaml").read_text())
        inherited = workflow.get("env", {}).get(SETTING)
        jobs = {name: job for name, job in workflow["jobs"].items() if macos_job(job)}
        assert len(jobs) >= 5, sorted(jobs)
        for name, job in jobs.items():
            with self.subTest(job=name):
                assert job.get("env", {}).get(SETTING, inherited) == deployment_target()

    def test_every_reusable_cli_build_passes_the_target_and_swift_derives_its_own(self) -> None:
        jobs = reusable_macos_builds()
        assert set(jobs) >= {("publish.yaml", "cli-release"), ("prepare-swift-release.yaml", "swift")}, sorted(jobs)
        for key, job in jobs.items():
            with self.subTest(job=key):
                expected = None if f"/{SWIFT_REUSABLE}@" in str(job["uses"]) else deployment_target()
                assert job.get("with", {}).get(CALLER_INPUT) == expected

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
