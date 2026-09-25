"""Exercise the Python wheel matrix's platform coverage and macOS deployment floor."""

from __future__ import annotations

import re
import unittest
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / ".github/workflows/publish.yaml"

# ~keep An allowlist rather than a regex on "intel"/"arm": GitHub's runner labels carry no
# ~keep consistent architecture marker (`macos-latest` and `macos-15` are arm64, `macos-13` is
# ~keep x86_64), so a rename must force a conscious update here instead of silently passing.
MACOS_RUNNER_ARCH = {
    "macos-latest": "arm64",
    "macos-15": "arm64",
    "macos-14": "arm64",
    "macos-15-intel": "x86_64",
    "macos-13": "x86_64",
}

# ~keep 11.0 is the arm64 floor, so anything above it would also move the published arm64 tag.
MAX_DEPLOYMENT_TARGET = (11, 0)


def _python_wheels_job() -> dict:
    workflow = yaml.safe_load(WORKFLOW.read_text(encoding="utf-8"))
    return workflow["jobs"]["python-wheels"]


def _build_step(job: dict) -> dict:
    steps = [s for s in job["steps"] if str(s.get("uses", "")).startswith("xberg-io/actions/build-python-wheels")]
    assert len(steps) == 1, f"expected exactly one build-python-wheels step, found {len(steps)}"
    return steps[0]


class PythonWheelMatrixTests(unittest.TestCase):
    def test_matrix_covers_both_macos_architectures(self) -> None:
        runners = _python_wheels_job()["strategy"]["matrix"]["os"]
        macos = [r for r in runners if r.startswith("macos")]
        assert macos, f"no macOS runner in the python-wheels matrix: {runners}"
        unknown = [r for r in macos if r not in MACOS_RUNNER_ARCH]
        assert not unknown, f"unrecognised macOS runner label(s) {unknown}; add them to MACOS_RUNNER_ARCH"
        covered = {MACOS_RUNNER_ARCH[r] for r in macos}
        assert covered == {"arm64", "x86_64"}, (
            f"python-wheels must publish a wheel for each macOS architecture; got {sorted(covered)} from {macos}. "
            "An Intel Mac otherwise falls back to the sdist, which needs a Rust toolchain."
        )

    def test_macos_wheels_pin_a_deployment_target_old_enough_to_install(self) -> None:
        env = _build_step(_python_wheels_job())["with"].get("cibw-environment-macos", "")
        match = re.search(r"MACOSX_DEPLOYMENT_TARGET=(\d+)(?:\.(\d+))?", env)
        assert match, (
            "cibw-environment-macos must pin MACOSX_DEPLOYMENT_TARGET; without it the wheel inherits "
            f"the runner's SDK version and pip rejects it on older macOS. Got: {env!r}"
        )
        target = (int(match.group(1)), int(match.group(2) or 0))
        assert target <= MAX_DEPLOYMENT_TARGET, (
            f"MACOSX_DEPLOYMENT_TARGET={match.group(0).split('=')[1]} is newer than "
            f"{'.'.join(map(str, MAX_DEPLOYMENT_TARGET))}, which would drop macOS users and move the arm64 tag."
        )


if __name__ == "__main__":
    unittest.main()
