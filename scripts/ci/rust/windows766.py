from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
from dataclasses import asdict, dataclass
from pathlib import Path

CONTROL = "page_open_diagnostics_capture_spawned_tasks_and_reject_malformed_evidence"
REAL = "pool_bounds_concurrent_navigations_to_max_pages"
TESTS = (CONTROL, REAL)
PROCESS_TIMEOUT_SECONDS = 90
SUMMARY = re.compile(
    r"test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored; (\d+) measured; (\d+) filtered out"
)


@dataclass(frozen=True)
class Counts:
    passed: int
    failed: int
    ignored: int
    measured: int

    @property
    def executed(self) -> int:
        return self.passed + self.failed


def parse_counts(text: str) -> Counts:
    matches = SUMMARY.findall(text)
    if len(matches) != 1:
        raise ValueError("expected exactly one libtest result summary")
    outcome, passed, failed, ignored, measured, _ = matches[0]
    counts = Counts(int(passed), int(failed), int(ignored), int(measured))
    if (outcome == "ok") != (counts.failed == 0):
        raise ValueError("libtest outcome and failed count disagree")
    if counts.executed != 1 or counts.ignored != 0 or counts.measured != 0:
        raise ValueError(f"expected one executed test and no skips/benchmarks: {counts}")
    if len(re.findall(r"(?m)^running 1 test$", text)) != 1:
        raise ValueError("expected exactly one libtest start count")
    if re.search(r"skipping .*|no usable Chrome was found", text, re.IGNORECASE):
        raise ValueError("a Chrome skip does not exercise the regression")
    return counts


def parse_listing(text: str, name: str) -> int:
    collected = [line for line in text.splitlines() if line.endswith(": test")]
    if collected != [f"{name}: test"]:
        raise ValueError(f"expected one exact collected test {name}: {collected}")
    return 1


def write_json(path: Path, data: object) -> None:
    path.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")


def read_json(path: Path) -> dict[str, object]:
    data = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(data, dict) or not all(isinstance(key, str) for key in data):
        raise ValueError(f"expected a JSON object: {path.name}")
    return data


def digest(path: Path) -> str:
    with path.open("rb") as handle:
        return hashlib.file_digest(handle, "sha256").hexdigest()


def command(*args: str) -> str:
    return subprocess.check_output(args, text=True, encoding="utf-8").strip()


def sources() -> dict[str, str]:
    paths = command(
        "git",
        "ls-files",
        "--cached",
        "--others",
        "--exclude-standard",
        "-z",
        "--",
        "Cargo.toml",
        "Cargo.lock",
        "rust-toolchain.toml",
        ".cargo/config.toml",
        "crates/crawlberg/Cargo.toml",
        "crates/crawlberg/src",
        "crates/crawlberg/tests/test_browser_pool_lifecycle.rs",
        "vendor/crawlberg-chromiumoxide/Cargo.toml",
        "vendor/crawlberg-chromiumoxide/Cargo.lock",
        "vendor/crawlberg-chromiumoxide/src",
    ).split("\0")
    selected = [name for name in paths if name]
    if len(selected) < 2:
        raise ValueError("source census examined fewer than two tracked files")
    return {name: digest(Path(name)) for name in selected}


@dataclass(frozen=True)
class SwiftChecksumProof:
    checksum: str
    placeholder_count: int
    original_package_sha256: str
    tagged_package_sha256: str


def validate_swift_checksum_advance(
    original: bytes, tagged: bytes, changed_paths: list[str], commits: int
) -> SwiftChecksumProof:
    if commits != 1 or changed_paths != ["Package.swift"]:
        raise ValueError("checksum advance must be one commit changing only Package.swift")
    placeholder = b"__ALEF_SWIFT_CHECKSUM__"
    if placeholder not in original:
        raise ValueError("original Package.swift has no checksum placeholder")
    prefix = original.split(placeholder, 1)[0]
    checksum = tagged[len(prefix) : len(prefix) + 64]
    if not re.fullmatch(rb"[0-9a-f]{64}", checksum):
        raise ValueError("Swift checksum must be exactly 64 lowercase hex bytes")
    if tagged != original.replace(placeholder, checksum):
        raise ValueError("tagged Package.swift changes bytes beyond one uniform checksum substitution")
    return SwiftChecksumProof(
        checksum.decode("ascii"),
        original.count(placeholder),
        hashlib.sha256(original).hexdigest(),
        hashlib.sha256(tagged).hexdigest(),
    )


def release_commit_proof(original_sha: str, tag_sha: str) -> SwiftChecksumProof | None:
    subprocess.run(["git", "merge-base", "--is-ancestor", original_sha, "HEAD"], check=True)
    if original_sha == tag_sha:
        return None
    subprocess.run(["git", "merge-base", "--is-ancestor", original_sha, tag_sha], check=True)
    commits = int(command("git", "rev-list", "--count", f"{original_sha}..{tag_sha}"))
    changed_paths = (
        subprocess.check_output(["git", "diff", "--no-renames", "--name-only", "-z", original_sha, tag_sha, "--"])
        .decode("utf-8")
        .split("\0")
    )
    original = subprocess.check_output(["git", "show", f"{original_sha}:Package.swift"])
    tagged = subprocess.check_output(["git", "show", f"{tag_sha}:Package.swift"])
    return validate_swift_checksum_advance(original, tagged, [path for path in changed_paths if path], commits)


def validate_release() -> dict[str, object]:
    run_id = os.environ["WINDOWS766_RELEASE_RUN"]
    tag = os.environ["WINDOWS766_RELEASE_TAG"]
    if not run_id.isdecimal() or not re.fullmatch(r"v\d+\.\d+\.\d+", tag):
        raise ValueError("release run must be numeric and tag must be vX.Y.Z")
    if os.environ.get("GITHUB_REF") != "refs/heads/main":
        raise ValueError("this diagnostic may run only on main")
    repository = os.environ["GITHUB_REPOSITORY"]
    if repository != "xberg-io/crawlberg":
        raise ValueError("unexpected diagnostic repository")
    data = json.loads(command("gh", "api", f"repos/{repository}/actions/runs/{run_id}"))
    if data.get("name") != "Publish Release" or data.get("conclusion") != "success" or data.get("head_branch") != tag:
        raise ValueError("the selected release publication run has not succeeded for this tag")
    original_sha = data.get("head_sha")
    if not isinstance(original_sha, str) or not re.fullmatch(r"[0-9a-f]{40}", original_sha):
        raise ValueError("publication run has no valid original commit SHA")
    tag_sha = command("git", "rev-parse", f"{tag}^{{commit}}")
    swift_proof = release_commit_proof(original_sha, tag_sha)
    return {
        "run_id": run_id,
        "tag": tag,
        "original_run_sha": original_sha,
        "final_tag_sha": tag_sha,
        "swift_checksum_advance": asdict(swift_proof) if swift_proof else None,
    }


def preserve_sources(output: Path, names: list[str], label: str) -> None:
    for name in names:
        source = Path(name)
        if source.is_file():
            destination = output / label / name
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, destination)


def patch_sources(patch: Path) -> list[str]:
    rows = command("git", "apply", "--numstat", str(patch)).splitlines()
    names = [row.split("\t", 2)[2] for row in rows]
    prefixes = ("crates/crawlberg/src/", "crates/crawlberg/tests/", "vendor/crawlberg-chromiumoxide/src/")
    if not 1 <= len(names) <= 10 or not all(name.startswith(prefixes) and name.endswith(".rs") for name in names):
        raise ValueError("diagnostic patch must change only bounded Rust source/test paths")
    return names


def compilation_environment() -> dict[str, str | None]:
    for name in ("RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER"):
        if os.environ.get(name):
            raise ValueError(f"diagnostic requires direct compilation with empty {name}")
    return {
        "cargo_profile": "test",
        "compiler_wrapper": "none",
        "CARGO_PROFILE_DEV_DEBUG": os.environ.get("CARGO_PROFILE_DEV_DEBUG"),
        "CARGO_INCREMENTAL": os.environ.get("CARGO_INCREMENTAL"),
        "CARGO_BUILD_JOBS": os.environ.get("CARGO_BUILD_JOBS"),
        "RUST_MIN_STACK": os.environ.get("RUST_MIN_STACK"),
    }


def prepare(output: Path) -> None:
    release = validate_release()
    build_environment = compilation_environment()
    chrome = Path(os.environ["CRAWLBERG_TEST_CHROME_PATH"]).resolve(strict=True)
    if not chrome.is_file():
        raise ValueError("Chrome is not an executable file")
    version = command(
        "powershell",
        "-NoProfile",
        "-Command",
        "(Get-Item -LiteralPath $env:CRAWLBERG_TEST_CHROME_PATH).VersionInfo.ProductVersion",
    )
    if not re.fullmatch(r"\d+\.\d+\.\d+\.\d+", version):
        raise ValueError(f"expected numeric Chrome ProductVersion: {version}")
    patch = Path("scripts/ci/rust/windows766-instrumentation.patch")
    baseline = sources()
    changed = patch_sources(patch)
    preserve_sources(output, [*changed, "Cargo.lock"], "baseline")
    shutil.copy2(patch, output / patch.name)
    subprocess.run(["git", "apply", "--check", str(patch)], check=True)
    subprocess.run(["git", "apply", str(patch)], check=True)
    preserve_sources(output, changed, "instrumented")
    metadata = {
        "release": release,
        "compilation_environment": build_environment,
        "head": command("git", "rev-parse", "HEAD"),
        "rustc": command("rustc", "-Vv"),
        "lock_sha256": digest(Path("Cargo.lock")),
        "chrome": {"path": str(chrome), "version": version, "sha256": digest(chrome)},
        "patch_sha256": digest(patch),
        "baseline_sources": baseline,
        "instrumented_sources": sources(),
        "runner": {
            key: os.environ.get(key) for key in ("RUNNER_OS", "RUNNER_ARCH", "ImageOS", "ImageVersion", "GITHUB_RUN_ID")
        },
        "expected_tests": list(TESTS),
        "expected_executions": 2,
    }
    write_json(output / "provenance.json", metadata)


def build(output: Path) -> None:
    args = [
        "cargo",
        "test",
        "--locked",
        "--profile",
        "test",
        "-p",
        "crawlberg",
        "--features",
        "browser,interact",
        "--test",
        "test_browser_pool_lifecycle",
        "--no-run",
        "--message-format=json-render-diagnostics",
    ]
    code, timed_out = run_process(args, output / "build", 2700)
    if code != 0 or timed_out:
        raise ValueError(f"build exit={code}; timed_out={timed_out}")
    artifacts: dict[str, dict[str, object]] = {}
    for line in (output / "build.stdout.log").read_text(encoding="utf-8").splitlines():
        data = json.loads(line)
        if (
            data.get("reason") == "compiler-artifact"
            and data.get("target", {}).get("name") == "test_browser_pool_lifecycle"
            and "test" in data.get("target", {}).get("kind", [])
            and isinstance(data.get("executable"), str)
        ):
            artifacts[data["executable"]] = {"profile": data.get("profile"), "features": data.get("features")}
    if len(artifacts) != 1:
        raise ValueError(f"expected one lifecycle executable, observed={len(artifacts)}")
    executable_name, artifact = artifacts.popitem()
    executable = Path(executable_name).resolve(strict=True)
    metadata = read_json(output / "provenance.json")
    if sources() != metadata["instrumented_sources"] or digest(Path("Cargo.lock")) != metadata["lock_sha256"]:
        raise ValueError("build changed instrumented source or lock bytes")
    write_json(
        output / "build.json",
        {
            "command": args,
            "exit_code": 0,
            "executable": str(executable),
            "sha256": digest(executable),
            "resolved_cargo_profile": artifact["profile"],
            "features": artifact["features"],
        },
    )


def run_process(args: list[str], stem: Path, timeout: int) -> tuple[int, bool]:
    with (
        log_path(stem, ".stdout.log").open("w", encoding="utf-8") as stdout,
        log_path(stem, ".stderr.log").open("w", encoding="utf-8") as stderr,
    ):
        process = subprocess.Popen(args, stdout=stdout, stderr=stderr)
        try:
            return process.wait(timeout=timeout), False
        except subprocess.TimeoutExpired:
            subprocess.run(
                ["taskkill", "/PID", str(process.pid), "/T", "/F"],
                check=False,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
            return process.wait(timeout=15), True


def log_path(stem: Path, suffix: str) -> Path:
    return stem.parent / (stem.name + suffix)


def combined(stem: Path) -> str:
    return "\n".join(
        log_path(stem, suffix).read_text(encoding="utf-8", errors="replace")
        for suffix in (".stdout.log", ".stderr.log")
    )


def run_test(output: Path, executable: str, name: str) -> dict[str, object]:
    listing = output / f"{name}.list"
    code, timed_out = run_process([executable, name, "--exact", "--list", "--format", "terse"], listing, 15)
    if code != 0 or timed_out:
        raise ValueError(f"test listing failed for {name}")
    collected = parse_listing(combined(listing), name)
    stem = output / name
    code, timed_out = run_process(
        [executable, name, "--exact", "--nocapture", "--test-threads=1"], stem, PROCESS_TIMEOUT_SECONDS
    )
    text = combined(stem)
    row: dict[str, object] = {"name": name, "collected": collected, "exit_code": code, "timed_out": timed_out}
    try:
        counts = parse_counts(text)
        if (counts.failed == 0) != (code == 0):
            raise ValueError("libtest counts and process exit disagree")
        row["counts"] = asdict(counts)
    except ValueError as error:
        row["invalid_evidence"] = str(error)
    row["diagnostic_snapshot_present"] = "windows766_diagnostics=" in text
    return row


def run_tests(output: Path) -> None:
    metadata = read_json(output / "build.json")
    executable = metadata["executable"]
    if not isinstance(executable, str) or digest(Path(executable)) != metadata["sha256"]:
        raise ValueError("compiled executable provenance is invalid")
    rows: list[dict[str, object]] = []
    for name in TESTS:
        rows.append(run_test(output, executable, name))
        write_json(output / "runs.json", {"expected_executions": 2, "observed_executions": len(rows), "runs": rows})
        if name == CONTROL and (rows[-1]["exit_code"] != 0 or rows[-1]["timed_out"] or "invalid_evidence" in rows[-1]):
            raise ValueError("capture control failed; real browser experiment not started")


def verify(output: Path) -> None:
    data = read_json(output / "runs.json")
    rows = data["runs"]
    if not isinstance(rows, list) or len(rows) != 2 or [row.get("name") for row in rows] != list(TESTS):
        raise ValueError("expected two ordered lifecycle executions, control then real")
    for row in rows:
        if row.get("timed_out") or "invalid_evidence" in row or row.get("collected") != 1:
            raise ValueError(f"inconclusive execution: {row}")
    if rows[0]["exit_code"] != 0 or not rows[1]["diagnostic_snapshot_present"]:
        raise ValueError("capture control or real-test diagnostic payload is missing")
    state = "passed_unreproduced" if rows[1]["exit_code"] == 0 else "real_test_failed"
    write_json(
        output / "verdict.json", {"state": state, "expected_executions": 2, "observed_executions": 2, "runs": rows}
    )
    print(json.dumps({"state": state, "expected_executions": 2, "observed_executions": 2}))
    if state != "passed_unreproduced":
        raise ValueError("real regression failed; inspect stage/scheduler/controller evidence before attributing cause")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("phase", choices=("prepare", "build", "run", "verify"))
    args = parser.parse_args()
    output = Path(os.environ["WINDOWS766_ARTIFACTS"]).resolve()
    output.mkdir(parents=True, exist_ok=True)
    phases = {"prepare": prepare, "build": build, "run": run_tests, "verify": verify}
    try:
        phases[args.phase](output)
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        write_json(
            output / f"{args.phase}.failure.json",
            {"phase": args.phase, "state": "failed", "error": str(error), "expected_executions": 2},
        )
        print(f"{args.phase}: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
