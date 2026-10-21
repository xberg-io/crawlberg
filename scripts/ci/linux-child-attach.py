"""Capture Linux recursive child-attach evidence without changing the security policy."""

import argparse
import hashlib
import json
import os
import re
import signal
import subprocess
import sys
import time
from pathlib import Path
from typing import NotRequired, TypedDict

RECOVERY_MARKER = "test proved unowned child-target retirement"
STRUCTURAL_RECOVERY_MARKER = "test proved unowned structural-target retirement"
RECOVERY_MARKERS = (RECOVERY_MARKER, STRUCTURAL_RECOVERY_MARKER)

FILTER = "browser_pool::tests::teardown_reliability::should_report_an_active_owned_browser_transport_failure"


STRUCTURAL_FILTER = (
    "browser_pool::tests::teardown_reliability::"
    "should_retire_an_owned_browser_with_a_live_page_without_controller_failure_logs"
)


STRUCTURAL_TEST_PREFIX = "ssrf_intercept::tests::unowned_structural::"
STRUCTURAL_TESTS = {
    STRUCTURAL_TEST_PREFIX + "a_never_authorized_detached_tab_requires_absence_before_its_exact_arm_can_retire",
    STRUCTURAL_TEST_PREFIX + "an_unowned_tab_census_rejects_live_invalid_unanswered_and_acknowledged_arms",
    STRUCTURAL_TEST_PREFIX + "a_detached_tab_cannot_retire_after_ownership_lineage_or_identity_changes",
}


class Result(TypedDict):
    """A native process receipt retaining streams after failure or timeout."""

    exit_code: int
    timed_out: bool
    stdout: str
    stderr: str


class Executable(TypedDict):
    """An immutable test artifact and its original Cargo package directory."""

    name: str
    path: str
    cwd: str
    sha256: str


class Row(TypedDict):
    """One expected test execution, including explicit evidence when it did no work."""

    phase: str
    expected_cases: int
    expected_ignored: int
    sha256: str
    ok: bool
    executed: bool
    log_prefix: str
    native_exit_code: NotRequired[int]
    timed_out: NotRequired[bool]
    filter: str | None
    recovery_receipts: dict[str, int]
    recovery_markers: int
    summaries: NotRequired[list[str]]
    error: NotRequired[str]


def save(directory: Path, name: str, value: object) -> None:
    """Persist each partial receipt before attempting another native test."""
    (directory / name).write_text(json.dumps(value, indent=2) + "\n")


def digest(path: str | Path) -> str:
    """Hash the exact executable bytes used for each replay."""
    with Path(path).open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def run(directory: Path, name: str, command: list[str], timeout: float, cwd: str | None = None) -> Result:
    """Capture streams directly to disk so a failed native process retains evidence."""
    with (directory / f"{name}.stdout.log").open("w") as stdout, (directory / f"{name}.stderr.log").open("w") as stderr:
        process = subprocess.Popen(command, cwd=cwd, stdout=stdout, stderr=stderr, start_new_session=True)
        timed_out = False
        try:
            code = process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            timed_out = True
            os.killpg(process.pid, signal.SIGKILL)
            code = process.wait(timeout=10)
    return {
        "exit_code": code,
        "timed_out": timed_out,
        "stdout": (directory / f"{name}.stdout.log").read_text(),
        "stderr": (directory / f"{name}.stderr.log").read_text(),
    }


def assert_result(result: Result, expected: int, ignored: int, *, full_suite: bool = False) -> None:
    """Reject missing work while preserving the baseline library's two explicit ignores."""
    summaries = re.findall(r"test result: (\w+)\. (\d+) passed; (\d+) failed; (\d+) ignored;", result["stdout"])
    if result["exit_code"] or result["timed_out"] or len(summaries) != 1:
        raise ValueError("Native test execution failed")
    status, passed, failed, skipped = summaries[0]
    if status != "ok" or int(passed) != expected - ignored or int(failed) or int(skipped) != ignored:
        raise ValueError(f"Expected {expected} cases with {ignored} explicit ignores")
    intentional_control = (
        "skipping detected_chrome_launch_errors_are_not_skippable: no Chrome executable: "
        "Could not auto detect a chrome executable"
    )
    streams = (result["stdout"] + "\n" + result["stderr"]).splitlines()
    for line in streams:
        if full_suite and line == intentional_control:
            continue
        if re.search(r"^skipping .*Chrome", line, re.IGNORECASE):
            raise ValueError("Chrome fixture skipped")


def assert_test_names(names: list[str]) -> None:
    """Require real registered tests and every exact regression/filter name before execution."""
    if len(names) != 2140 or len(set(names)) != 2140:
        raise ValueError("Expected 2140 unique registered test names")
    structural = {name for name in names if name.startswith(STRUCTURAL_TEST_PREFIX)}
    if structural != STRUCTURAL_TESTS:
        raise ValueError("Expected exactly the three structural regression test names")
    if not {FILTER, STRUCTURAL_FILTER}.issubset(names):
        raise ValueError("Expected both exact real-browser filter names")


def enumerate_tests(directory: Path, executable: Executable) -> None:
    """Preserve the immutable compiled binary's actual list before accepting its counts."""
    if digest(executable["path"]) != executable["sha256"]:
        raise ValueError("Enumeration executable bytes changed")
    result = run(directory, "enumerate-core", [executable["path"], "--list"], 30, cwd=executable["cwd"])
    names = re.findall(r"^(.+): test$", result["stdout"], re.MULTILINE)
    save(
        directory,
        "enumeration.json",
        {
            "expected_names": 2140,
            "observed_names": len(names),
            "unique_names": len(set(names)),
            "expected_structural_names": sorted(STRUCTURAL_TESTS),
            "expected_filters": [FILTER, STRUCTURAL_FILTER],
            "sha256": executable["sha256"],
            "native_exit_code": result["exit_code"],
            "timed_out": result["timed_out"],
            "names": names,
        },
    )
    if result["exit_code"] or result["timed_out"]:
        raise ValueError("Native test enumeration failed; complete list logs preserved")
    assert_test_names(names)
    if digest(executable["path"]) != executable["sha256"]:
        raise ValueError("Enumeration executable bytes changed")
    sys.stdout.write("Registered tests: expected2140 observed2140; exact3regressions and2filters verified\n")


def build(directory: Path) -> None:
    """Compile one all-feature unit binary without the integration test executables."""
    command = ["cargo", "test", "-p", "crawlberg", "--all-features", "--lib", "--no-run", "--message-format=json"]
    result = run(directory, "build-core", command, 1200)
    if result["exit_code"] or result["timed_out"]:
        raise ValueError("Core library build failed; full logs preserved")
    artifacts: list[Executable] = []
    for line in result["stdout"].splitlines():
        if not line.startswith("{"):
            continue
        item = json.loads(line)
        if (
            item.get("reason") == "compiler-artifact"
            and item.get("executable")
            and item["profile"]["test"]
            and item["target"]["name"] == "crawlberg"
        ):
            artifacts.append(
                {
                    "name": "crawlberg",
                    "path": item["executable"],
                    "cwd": str(Path(item["manifest_path"]).parent),
                    "sha256": digest(item["executable"]),
                }
            )
    if len(artifacts) != 1:
        raise ValueError(f"Expected one core library executable; observed {len(artifacts)}")
    save(directory, "executables.json", artifacts)
    enumerate_tests(directory, artifacts[0])
    sys.stdout.write("Expected core executables: 1; observed: 1\n")
    sys.stdout.flush()


def run_case(
    directory: Path, executable: Executable, index: int, full: bool, deadline: float, *, filter_name: str
) -> Row:
    """Run a case with original package cwd and an unchanged executable hash."""
    expected, ignored = (2140, 2) if full else (1, 0)
    name = f"{index:02d}-" + ("full-core" if full else "focused-child-attach")
    row: Row = {
        "phase": "full" if full else "focused",
        "expected_cases": expected,
        "expected_ignored": ignored,
        "sha256": executable["sha256"],
        "ok": False,
        "executed": False,
        "log_prefix": name,
        "recovery_markers": 0,
        "recovery_receipts": dict.fromkeys(RECOVERY_MARKERS, 0),
        "filter": None if full else filter_name,
    }
    try:
        if digest(executable["path"]) != executable["sha256"]:
            raise ValueError("Executable bytes changed")
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise ValueError("Diagnostic budget exhausted; case not executed")
        arguments = ["--nocapture", "--test-threads=1"] + ([] if full else ["--exact", filter_name])
        result = run(
            directory,
            name,
            [executable["path"], *arguments],
            min(900 if full else 90, remaining),
            cwd=executable["cwd"],
        )
        row.update(executed=True, native_exit_code=result["exit_code"], timed_out=result["timed_out"])
        row["recovery_receipts"] = {
            marker: (result["stdout"] + result["stderr"]).count(marker) for marker in RECOVERY_MARKERS
        }
        row["recovery_markers"] = sum(row["recovery_receipts"].values())
        row["summaries"] = re.findall(r"test result: [^\r\n]+", result["stdout"])
        assert_result(result, expected, ignored, full_suite=full)
        row["ok"] = True
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        row["error"] = str(error)
    sys.stdout.write(json.dumps(row) + "\n")
    sys.stdout.flush()
    return row


def save_probe_rows(directory: Path, rows: list[Row]) -> None:
    """Persist actual footers and focused recovery receipts before the next replay."""
    save(
        directory,
        "results.json",
        {
            "expected_rows": 121,
            "observed_rows": len(rows),
            "executed_rows": sum(row["executed"] for row in rows),
            "expected_registered_cases": 2260,
            "expected_passed": 2258,
            "expected_ignored": 2,
            "expected_focused_per_filter": {FILTER: 60, STRUCTURAL_FILTER: 60},
            "observed_focused_per_filter": {
                name: sum(row["filter"] == name for row in rows) for name in (FILTER, STRUCTURAL_FILTER)
            },
            "recovery_markers": RECOVERY_MARKERS,
            "focused_recovery_receipts": {
                marker: sum(row["recovery_receipts"][marker] for row in rows[1:]) for marker in RECOVERY_MARKERS
            },
            "recovery_marker_count": sum(row["recovery_markers"] for row in rows),
            "focused_recovery_marker_count": sum(row["recovery_markers"] for row in rows[1:]),
            "rows": rows,
        },
    )


def probe(directory: Path) -> None:
    """Preserve full-suite order before sixty alternating reproductions per real-browser case."""
    artifacts = json.loads((directory / "executables.json").read_text())
    if len(artifacts) != 1 or artifacts[0]["name"] != "crawlberg":
        raise ValueError("Expected exactly one core library executable receipt")
    chrome = os.environ.get("CHROME")
    if not chrome or not Path(chrome).is_file():
        raise ValueError("CHROME must name the prepared snap executable")
    browser = run(directory, "browser-version", [chrome, "--version"], 30)
    if browser["exit_code"] or not re.search(r"\d+\.\d+\.\d+\.\d+", browser["stdout"]):
        raise ValueError("Browser version unavailable; see browser-version logs")
    save(
        directory,
        "browser.json",
        {"path": chrome, "version_output": browser["stdout"].strip(), "sha256": digest(chrome)},
    )
    if re.search(r"\b155\.0\.8059\.84\b", browser["stdout"]) is None:
        raise ValueError("Same-snap verification requires Chromium 155.0.8059.84; actual browser receipt preserved")
    os.environ.update(RUST_BACKTRACE="full", RUST_TEST_THREADS="1")
    rows: list[Row] = []
    deadline = time.monotonic() + 18 * 60
    for index in range(121):
        filter_name = FILTER if index % 2 else STRUCTURAL_FILTER
        rows.append(run_case(directory, artifacts[0], index, index == 0, deadline, filter_name=filter_name))
        save_probe_rows(directory, rows)
    if digest(artifacts[0]["path"]) != artifacts[0]["sha256"]:
        raise ValueError("Final executable bytes changed")
    if len(rows) != 121 or not all(row["ok"] for row in rows):
        raise ValueError("Diagnostic failed; inspect all preserved rows/logs before attributing a target type")
    if any(sum(row["filter"] == name for row in rows) != 60 for name in (FILTER, STRUCTURAL_FILTER)):
        raise ValueError("Expected exactly sixty real-browser replays per filter")
    assert_recovery_count(sum(row["recovery_markers"] for row in rows[1:]))


def assert_recovery_count(count: int) -> None:
    """Reject green test counts when the candidate recovery path was never observed."""
    if count < 1:
        raise ValueError("Candidate recovery was not observed: expected at least one retirement receipt")


def _rejects(case: Result, expected: int = 1, ignored: int = 0, *, full_suite: bool = False) -> bool:
    try:
        assert_result(case, expected, ignored, full_suite=full_suite)
    except ValueError:
        return True
    return False


def assert_bad_test_names(names: list[str], reason: str) -> None:
    """Require each enumeration negative to fail for its intended guard."""
    try:
        assert_test_names(names)
    except ValueError as error:
        if str(error) != reason:
            raise ValueError("Enumeration control rejected for the wrong reason") from error
    else:
        raise ValueError("Enumeration negative control was accepted")


def test_enumeration_controls() -> None:
    """Reject zero work and a same-count rename of each required regression/filter."""
    required = sorted(STRUCTURAL_TESTS | {FILTER, STRUCTURAL_FILTER})
    names = required + [f"fixture::{index}" for index in range(2140 - len(required))]
    assert_test_names(names)
    cases = [([], "Expected 2140 unique registered test names")]
    for name in required:
        wrong = ["fixture::renamed" if value == name else value for value in names]
        reason = (
            "Expected exactly the three structural regression test names"
            if name in STRUCTURAL_TESTS
            else ("Expected both exact real-browser filter names")
        )
        cases.append((wrong, reason))
    for candidate, reason in cases:
        assert_bad_test_names(candidate, reason)
    sys.stdout.write("Enumeration controls: 1 positive; 1 zero-work and5exact-name negatives\n")


def self_test() -> None:
    """Prove the count and skip guards reject known zero-work results."""
    test_enumeration_controls()
    positive: Result = {
        "exit_code": 0,
        "timed_out": False,
        "stdout": "test result: ok. 1 passed; 0 failed; 0 ignored;",
        "stderr": "",
    }
    assert_result(positive, 1, 0)
    full: Result = {**positive, "stdout": "test result: ok. 2138 passed; 0 failed; 2 ignored;"}
    assert_result(full, 2140, 2)
    control_line = (
        "skipping detected_chrome_launch_errors_are_not_skippable: no Chrome executable: "
        "Could not auto detect a chrome executable"
    )
    full_control: Result = {**full, "stderr": control_line}
    assert_result(full_control, 2140, 2, full_suite=True)
    negatives: list[Result] = [
        {**positive, "stdout": "test result: ok. 0 passed; 0 failed; 0 ignored;"},
        {**positive, "exit_code": 101},
        {**positive, "stdout": "test result: ok. 1 passed; 0 failed; 1 ignored;"},
        {**positive, "stderr": "skipping fixture because no usable Chrome was found"},
        {**positive, "stderr": control_line},
    ]
    rejected = sum(_rejects(case) for case in negatives)
    full_skip: Result = {**full, "stderr": control_line + " altered"}
    rejected += _rejects(full_skip, 2140, 2, full_suite=True)
    if rejected != 6:
        raise ValueError(f"Expected six negative controls; observed {rejected}")
    assert_recovery_count(1)
    try:
        assert_recovery_count(0)
    except ValueError as error:
        if str(error) != "Candidate recovery was not observed: expected at least one retirement receipt":
            raise
    else:
        raise ValueError("Missing-recovery negative control was accepted")
    sys.stdout.write("Harness controls: 4 positives; 7 expected failures\n")


def main() -> None:
    """Select a diagnostic phase without modifying the production source tree."""
    parser = argparse.ArgumentParser()
    parser.add_argument("--mode", choices=["build", "probe", "self-test"], required=True)
    arguments = parser.parse_args()
    if arguments.mode == "self-test":
        self_test()
        return
    output = os.environ.get("CRAWLBERG_LINUX_DIAGNOSTICS_DIR")
    if not output:
        raise ValueError("Diagnostic output directory must be explicit")
    directory = Path(output)
    directory.mkdir(parents=True, exist_ok=True)
    (build if arguments.mode == "build" else probe)(directory)


if __name__ == "__main__":
    main()
