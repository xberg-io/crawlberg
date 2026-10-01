"""Run fixed document-read proof arms and bank hook-checked additive commits."""

from __future__ import annotations

import base64
import hashlib
import json
import os
import re
import subprocess
import sys
import zlib
from pathlib import Path, PurePosixPath
from typing import Any

REPOSITORY = "xberg-io/crawlberg"
ORIGINAL_HEAD = "03ca6e76940419ef98e71d705d4b115e23ab66ad"
INTEGRATION = "be88bb30fb7e77872ab31b27cfb9294a467ccd35"
MERGE_BASE = "b632f342f9e798cf3f8d560fb192fee7e537f0c3"
IDENTITY = "Tobias Perelstein <5562156+tobocop2@users.noreply.github.com>"
HOOK_DIGESTS = {
    "pre-commit": "2adbad06827027bdc2bd104bc40688e9730f2f8a31395cb6eb0ece14900766f8",
    "commit-msg": "a02bea6b370eb5ce96a9996432d784037dc7ec95d75e48d821d9417ea9546dcb",
}
PRODUCTION_PATHS = {
    "crates/crawlberg/src/browser/navigation.rs",
    "crates/crawlberg/src/interact/chromiumoxide.rs",
    "crates/crawlberg/src/chrome_frame.rs",
}
FIXED_PATHS = PRODUCTION_PATHS | {
    "crates/crawlberg/src/chrome_frame/read_timeout_tests.rs",
    "crates/crawlberg/tests/test_browser_document_read_timeout.rs",
    "crates/crawlberg/tests/test_browser_document_status.rs",
    "CHANGELOG.md",
    "docs-site/src/content/docs/changelog.md",
}
ANSI = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
RESULT = re.compile(
    r"test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; "
    r"(\d+) ignored; (\d+) measured; (\d+) filtered out"
)
HEX40 = re.compile(r"[0-9a-f]{40}\Z")
HEX64 = re.compile(r"[0-9a-f]{64}\Z")
TEST_NAME = re.compile(r"[a-zA-Z_][a-zA-Z_0-9:]*\Z")
ROOT = Path(__file__).resolve().parents[2]
EVIDENCE = ROOT / "takeover-evidence"
PATCH_DATA: dict[str, bytes] = {}


def refuse(message: str) -> None:
    raise ValueError(message)


def require(condition: bool, message: str) -> None:
    if not condition:
        refuse(message)


def object_fields(value: Any, required: set[str], label: str) -> dict[str, Any]:
    require(isinstance(value, dict), f"{label} is not an object")
    require(set(value) == required, f"{label} has missing or extra fields")
    return value


def sha256(content: bytes) -> str:
    return hashlib.sha256(content).hexdigest()


def execute(name: str, command: list[str], cwd: Path, *, check: bool = True) -> subprocess.CompletedProcess[str]:
    log = EVIDENCE / f"{name}.log"
    result = subprocess.run(command, cwd=cwd, capture_output=True, text=True, check=False)
    log.write_text(result.stdout + result.stderr)
    receipt = {"name": name, "command": command, "cwd": str(cwd), "rc": result.returncode, "log": str(log)}
    (EVIDENCE / f"{name}.json").write_text(json.dumps(receipt, indent=2) + "\n")
    print(f"STEP_RESULT name={name} rc={result.returncode}", flush=True)
    if check:
        require(result.returncode == 0, f"{name} failed; see {log}")
    return result


def git(cwd: Path, *arguments: str) -> str:
    result = subprocess.run(["git", *arguments], cwd=cwd, capture_output=True, text=True, check=False)
    require(result.returncode == 0, f"git {' '.join(arguments)}: {result.stderr.strip()}")
    return result.stdout.strip()


def blob(cwd: Path, relative: str) -> str | None:
    result = subprocess.run(
        ["git", "rev-parse", "--verify", f":{relative}"], cwd=cwd, capture_output=True, text=True, check=False
    )
    if result.returncode:
        return None
    require(HEX40.fullmatch(result.stdout.strip()) is not None, f"invalid blob for {relative}")
    return result.stdout.strip()


def safe_path(value: Any) -> str:
    require(isinstance(value, str), "path is not a string")
    path = PurePosixPath(value)
    require(
        not path.is_absolute() and path.parts and all(part not in {".", ".."} for part in path.parts), "unsafe path"
    )
    require(str(path) == value and "\\" not in value and "\x00" not in value, "noncanonical path")
    return value


def validate_test(value: Any) -> dict[str, Any]:
    test = object_fields(value, {"target", "name", "fingerprint"}, "test")
    require(
        test["target"] in {"lib", "test_browser_document_read_timeout", "test_browser_document_status"},
        "unknown target",
    )
    require(isinstance(test["name"], str) and TEST_NAME.fullmatch(test["name"]) is not None, "invalid test name")
    require(
        isinstance(test["fingerprint"], str) and 8 <= len(test["fingerprint"]) <= 400, "invalid failure fingerprint"
    )
    return test


def validate_patch(value: Any, allowed: set[str]) -> dict[str, Any]:
    patch = object_fields(value, {"file", "sha256", "paths"}, "patch")
    require(safe_path(patch["file"]).startswith("pr568-proof/"), "patch outside immutable data directory")
    require(HEX64.fullmatch(patch["sha256"]) is not None, "invalid patch SHA-256")
    require(isinstance(patch["paths"], list) and patch["paths"], "empty patch path list")
    seen = set()
    for row in patch["paths"]:
        object_fields(row, {"path", "before", "after", "mode"}, "patch path")
        path = safe_path(row["path"])
        require(path in allowed and path not in seen, f"unapproved or duplicate patch path {path}")
        seen.add(path)
        require(row["mode"] in {"100644", "100755"}, "unsupported patch mode")
        for side in ("before", "after"):
            require(row[side] is None or HEX40.fullmatch(row[side]) is not None, "invalid patch blob")
        require(row["before"] != row["after"], "patch does not change its advertised blob")
    return patch


def validate_arm(value: Any, allowed: set[str]) -> dict[str, Any]:
    arm = object_fields(value, {"patches", "expected_tree", "tests"}, "arm")
    require(isinstance(arm["patches"], list) and arm["patches"], "empty arm patch sequence")
    arm["patches"] = [validate_patch(patch, allowed) for patch in arm["patches"]]
    require(HEX40.fullmatch(arm["expected_tree"]) is not None, "invalid arm tree")
    require(isinstance(arm["tests"], list) and arm["tests"], "empty arm test manifest")
    arm["tests"] = [validate_test(test) for test in arm["tests"]]
    require(len({(test["target"], test["name"]) for test in arm["tests"]}) == len(arm["tests"]), "duplicate test")
    return arm


def fetch_manifest() -> tuple[dict[str, Any], str]:
    commit = os.environ.get("PATCH_COMMIT", "")
    payload = os.environ.get("PATCH_PAYLOAD", "")
    digest = os.environ.get("PATCH_DIGEST", "")
    require(HEX64.fullmatch(digest) is not None, "PATCH_DIGEST must be SHA-256")
    require(bool(commit) != bool(payload), "choose exactly one immutable data source")
    if payload:
        require(len(payload) <= 60000, "encoded patch payload too large")
        decompressor = zlib.decompressobj()
        decoded = decompressor.decompress(base64.b64decode(payload, validate=True), 2 * 1024 * 1024 + 1)
        require(
            decompressor.eof and not decompressor.unused_data and len(decoded) <= 2 * 1024 * 1024,
            "invalid compressed payload",
        )
        require(sha256(decoded) == digest, "decoded payload digest mismatch")
        bundle = object_fields(json.loads(decoded), {"manifest", "patches"}, "payload")
        require(isinstance(bundle["patches"], dict), "patch data must be an object")
        for path, data in bundle["patches"].items():
            require(safe_path(path).startswith("pr568-proof/"), "data path outside proof directory")
            PATCH_DATA[path] = base64.b64decode(data, validate=True)
        content = json.dumps(bundle["manifest"], sort_keys=True).encode()
        (EVIDENCE / "dispatch-data.json").write_bytes(decoded)
        commit = "dispatch-data:" + digest
    else:
        require(HEX40.fullmatch(commit) is not None, "PATCH_COMMIT must be an immutable commit")
        execute("fetch-data", ["git", "fetch", "--no-tags", "origin", commit], ROOT)
        require(git(ROOT, "rev-parse", "--verify", f"{commit}^{{commit}}") == commit, "patch commit moved")
        content = subprocess.check_output(["git", "show", f"{commit}:pr568-proof/manifest.json"], cwd=ROOT)
        require(sha256(content) == digest, "manifest digest mismatch")
    (EVIDENCE / "manifest.json").write_bytes(content)
    manifest = object_fields(
        json.loads(content),
        {
            "version",
            "head",
            "original_head",
            "integration",
            "merge_base",
            "allowed_paths",
            "candidate",
            "main_control",
            "integration_control",
            "mutations",
            "bank_message",
        },
        "manifest",
    )
    require(manifest["version"] == 1, "unknown manifest version")
    require(manifest["original_head"] == ORIGINAL_HEAD, "unexpected original head")
    require(manifest["integration"] == INTEGRATION and manifest["merge_base"] == MERGE_BASE, "base pins changed")
    require(HEX40.fullmatch(manifest["head"]) is not None, "invalid head")
    require(isinstance(manifest["allowed_paths"], list) and manifest["allowed_paths"], "empty allowed paths")
    allowed = {safe_path(path) for path in manifest["allowed_paths"]}
    require(allowed <= FIXED_PATHS and len(allowed) == len(manifest["allowed_paths"]), "unapproved allowed paths")
    manifest["candidate"] = validate_arm(manifest["candidate"], allowed)
    manifest["main_control"] = validate_arm(manifest["main_control"], allowed)
    manifest["integration_control"] = validate_arm(manifest["integration_control"], allowed)
    require(isinstance(manifest["mutations"], list) and manifest["mutations"], "zero manual mutation arms")
    names = set()
    for mutation in manifest["mutations"]:
        object_fields(mutation, {"name", "patch", "expected_tree", "tests"}, "mutation")
        require(
            TEST_NAME.fullmatch(mutation["name"]) is not None and ":" not in mutation["name"], "unsafe mutation name"
        )
        require(mutation["name"] not in names, "duplicate mutation name")
        names.add(mutation["name"])
        mutation["patch"] = validate_patch(mutation["patch"], PRODUCTION_PATHS)
        require(HEX40.fullmatch(mutation["expected_tree"]) is not None, "invalid mutation tree")
        require(isinstance(mutation["tests"], list) and mutation["tests"], "empty mutation tests")
        mutation["tests"] = [validate_test(test) for test in mutation["tests"]]
    message = manifest["bank_message"]
    require(
        isinstance(message, str) and re.match(r"(fix|refactor|test|docs|chore)(\([a-z-]+\))?: .+", message),
        "invalid bank message",
    )
    require("Co-Authored-By" not in message and "Generated with" not in message, "forbidden authorship footer")
    for pin in (manifest["head"], ORIGINAL_HEAD, INTEGRATION, MERGE_BASE):
        git(ROOT, "rev-parse", "--verify", f"{pin}^{{commit}}")
    execute("original-ancestor", ["git", "merge-base", "--is-ancestor", ORIGINAL_HEAD, manifest["head"]], ROOT)
    require(git(ROOT, "merge-base", manifest["head"], INTEGRATION) == MERGE_BASE, "actual merge-base changed")
    return manifest, commit


def patch_bytes(commit: str, descriptor: dict[str, Any], name: str) -> Path:
    if commit.startswith("dispatch-data:"):
        require(descriptor["file"] in PATCH_DATA, "advertised patch absent from dispatch")
        raw = PATCH_DATA[descriptor["file"]]
    else:
        raw = subprocess.check_output(["git", "show", f"{commit}:{descriptor['file']}"], cwd=ROOT)
    require(sha256(raw) == descriptor["sha256"], f"patch hash mismatch: {descriptor['file']}")
    path = EVIDENCE / f"{name}.patch"
    path.write_bytes(raw)
    return path


def apply_patch(cwd: Path, commit: str, descriptor: dict[str, Any], name: str) -> None:
    path = patch_bytes(commit, descriptor, name)
    advertised = {row["path"] for row in descriptor["paths"]}
    summary = git(cwd, "apply", "--numstat", str(path))
    observed = {line.split("\t", 2)[2] for line in summary.splitlines()}
    require(observed == advertised and observed, f"{name} path census mismatch")
    for row in descriptor["paths"]:
        require(blob(cwd, row["path"]) == row["before"], f"{name} before blob mismatch: {row['path']}")
        absolute = cwd / row["path"]
        require(
            not absolute.is_symlink() and all(not parent.is_symlink() for parent in absolute.parents if parent != cwd),
            "patch path is a symlink",
        )
    execute(f"{name}-check", ["git", "apply", "--check", "--index", str(path)], cwd)
    execute(f"{name}-apply", ["git", "apply", "--index", str(path)], cwd)
    for row in descriptor["paths"]:
        require(blob(cwd, row["path"]) == row["after"], f"{name} after blob mismatch: {row['path']}")
        if row["after"] is not None:
            mode = git(cwd, "ls-files", "--stage", "--", row["path"]).split()[0]
            require(mode == row["mode"], f"{name} mode mismatch")
    require(not git(cwd, "diff", "--name-only"), f"{name} unstaged changes")
    (EVIDENCE / f"{name}-provenance.json").write_text(
        json.dumps({"patch": descriptor, "tree": git(cwd, "write-tree")}, indent=2) + "\n"
    )


def reconstruct(name: str, pin: str, arm: dict[str, Any], commit: str) -> Path:
    base = (
        Path(os.environ["RUNNER_TEMP"]).resolve()
        / f"pr568-{os.environ['GITHUB_RUN_ID']}-{os.environ['GITHUB_RUN_ATTEMPT']}"
    )
    base.mkdir(exist_ok=True)
    cwd = base / name
    require(not cwd.exists(), f"arm directory already exists: {cwd}")
    execute(f"{name}-worktree", ["git", "worktree", "add", "--detach", str(cwd), pin], ROOT)
    require(not git(cwd, "status", "--porcelain"), f"{name} starts dirty")
    for index, patch in enumerate(arm["patches"]):
        apply_patch(cwd, commit, patch, f"{name}-{index}")
    require(git(cwd, "write-tree") == arm["expected_tree"], f"{name} tree mismatch")
    return cwd


def run_test(cwd: Path, test: dict[str, Any], name: str, red: bool, *, feature: str | None = None) -> None:
    command = ["cargo", "test", "-p", "crawlberg"]
    command.extend(["--all-features"] if feature is None else ["--no-default-features", "--features", feature])
    command.extend(["--lib"] if test["target"] == "lib" else ["--test", test["target"]])
    command.extend([test["name"], "--", "--exact", "--nocapture"])
    result = execute(name, command, cwd, check=False)
    text = ANSI.sub("", result.stdout + result.stderr)
    rows = list(RESULT.finditer(text))
    require(len(rows) == 1, f"{name} missing or ambiguous test result")
    status, passed, failed, ignored, _measured, filtered = rows[0].groups()
    counts = {"passed": int(passed), "failed": int(failed), "ignored": int(ignored), "filtered": int(filtered)}
    require(
        counts["passed"] + counts["failed"] == 1 and counts["ignored"] == 0,
        f"{name} selected zero, ignored, or multiple cases",
    )
    require(re.search(r"skipping|skip.*chrome|missing chrome", text, re.IGNORECASE) is None, f"{name} silently skipped")
    if red:
        require(result.returncode != 0 and status == "FAILED" and counts["failed"] == 1, f"{name} is not assertion-red")
        require(test["fingerprint"] in text, f"{name} red fingerprint absent")
    else:
        require(result.returncode == 0 and status == "ok" and counts["passed"] == 1, f"{name} is not green")
    (EVIDENCE / f"{name}-counts.json").write_text(json.dumps(counts, indent=2) + "\n")


def control_arms(manifest: dict[str, Any], commit: str) -> None:
    for label, pin in (("main_control", MERGE_BASE), ("integration_control", INTEGRATION)):
        arm = manifest[label]
        cwd = reconstruct(label, pin, arm, commit)
        for index, test in enumerate(arm["tests"]):
            run_test(cwd, test, f"{label}-test-{index}", True)
    candidate = reconstruct("mutation-candidate", INTEGRATION, manifest["candidate"], commit)
    for index, test in enumerate(manifest["candidate"]["tests"]):
        run_test(candidate, test, f"mutation-control-{index}", False)
    for index, mutation in enumerate(manifest["mutations"]):
        label = f"manual-{index}-{mutation['name']}"
        apply_patch(candidate, commit, mutation["patch"], label)
        require(git(candidate, "write-tree") == mutation["expected_tree"], f"{label} tree mismatch")
        for position, test in enumerate(mutation["tests"]):
            run_test(candidate, test, f"{label}-test-{position}", True)
        path = EVIDENCE / f"{label}.patch"
        execute(f"{label}-restore-check", ["git", "apply", "--reverse", "--check", "--index", str(path)], candidate)
        execute(f"{label}-restore", ["git", "apply", "--reverse", "--index", str(path)], candidate)
        require(
            git(candidate, "write-tree") == manifest["candidate"]["expected_tree"], f"{label} restoration tree mismatch"
        )
    diff = EVIDENCE / "production.diff"
    diff.write_text(git(candidate, "diff", "--cached", INTEGRATION, "--", *sorted(PRODUCTION_PATHS)) + "\n")
    require(diff.stat().st_size > 1, "empty production diff")
    execute("mutants-version", ["cargo", "mutants", "--version"], candidate)
    output = EVIDENCE / "cargo-mutants"
    result = execute(
        "cargo-mutants",
        [
            "cargo",
            "mutants",
            "-p",
            "crawlberg",
            "--in-diff",
            str(diff),
            "--all-features",
            "--test-tool",
            "cargo",
            "--in-place",
            "--output",
            str(output),
            "--",
            "--lib",
            "read_timeout_",
            "--",
            "--nocapture",
        ],
        candidate,
        check=False,
    )
    outcomes = list(output.rglob("outcomes.json"))
    require(len(outcomes) == 1, "cargo-mutants result missing")
    summary = json.loads(outcomes[0].read_text())
    require(
        int(summary.get("total_mutants", 0)) > 0 and int(summary.get("caught", 0)) > 0, "zero powered generated mutants"
    )
    require(
        result.returncode == 0 and int(summary.get("missed", -1)) == 0 and int(summary.get("timeout", -1)) == 0,
        "generated mutation gaps remain",
    )
    require(
        git(candidate, "write-tree") == manifest["candidate"]["expected_tree"]
        and not git(candidate, "diff", "--name-only"),
        "mutation tool left changes",
    )


def candidate_gate(manifest: dict[str, Any], commit: str) -> None:
    cwd = reconstruct("candidate", INTEGRATION, manifest["candidate"], commit)
    for index, test in enumerate(manifest["candidate"]["tests"]):
        run_test(cwd, test, f"candidate-test-{index}", False)
        if test["target"] == "lib" and "interact" not in test["name"]:
            for feature in ("browser", "browser-chromiumoxide"):
                run_test(cwd, test, f"candidate-{feature}-{index}", False, feature=feature)
    execute(
        "workspace-clippy",
        [
            "cargo",
            "clippy",
            "--workspace",
            "--exclude",
            "crawlberg-ffi",
            "--exclude",
            "crawlberg-py",
            "--all-features",
            "--",
            "-D",
            "warnings",
        ],
        cwd,
    )
    load_index = 0
    load_log = EVIDENCE / f"parallel-suite-{load_index}.log"
    stream = load_log.open("w")
    process = subprocess.Popen(["task", "rust:test:ci"], cwd=cwd, stdout=stream, stderr=subprocess.STDOUT)
    try:
        for round_index in range(20):
            for index, test in enumerate(manifest["candidate"]["tests"]):
                if process.poll() is not None:
                    stream.close()
                    validate_suite(load_log, process.returncode, load_index)
                    load_index += 1
                    load_log = EVIDENCE / f"parallel-suite-{load_index}.log"
                    stream = load_log.open("w")
                    process = subprocess.Popen(
                        ["task", "rust:test:ci"], cwd=cwd, stdout=stream, stderr=subprocess.STDOUT
                    )
                run_test(cwd, test, f"repeat-{round_index}-{index}", False)
    finally:
        suite_rc = process.wait()
        stream.close()
    validate_suite(load_log, suite_rc, load_index)
    execute("format", ["cargo", "fmt", "--all", "--", "--check"], cwd)
    execute("mirror", ["bash", "scripts/ci/check-changelog-mirror.sh"], cwd)
    execute(
        "quality",
        [
            "poly",
            "lint",
            "--no-workspace",
            "--no-cache",
            "--only",
            "quality",
            "--format",
            "json",
            *sorted(PRODUCTION_PATHS),
        ],
        cwd,
    )
    require(
        git(cwd, "write-tree") == manifest["candidate"]["expected_tree"] and not git(cwd, "diff", "--name-only"),
        "gate changed candidate bytes",
    )


def validate_suite(log: Path, rc: int, index: int) -> None:
    print(f"STEP_RESULT name=parallel-suite-{index} rc={rc}", flush=True)
    require(rc == 0, f"parallel-suite-{index} failed")
    rows = list(RESULT.finditer(ANSI.sub("", log.read_text())))
    require(
        rows and sum(int(row.group(2)) for row in rows) > 0 and all(int(row.group(3)) == 0 for row in rows),
        "parallel suite has no complete green result",
    )


def bank(manifest: dict[str, Any], commit: str) -> None:
    require(os.environ.get("RUNNER_OS") == "Linux", "banking is Linux-only")
    for variable in (
        "POLY_SKIP_HOOKS",
        "SKIP",
        "PREK_SKIP",
        "GIT_AUTHOR_NAME",
        "GIT_AUTHOR_EMAIL",
        "GIT_COMMITTER_NAME",
        "GIT_COMMITTER_EMAIL",
    ):
        require(not os.environ.get(variable), f"forbidden hook/identity override: {variable}")
    candidate = reconstruct("bank-candidate", INTEGRATION, manifest["candidate"], commit)
    empty = {"patches": [], "expected_tree": git(ROOT, "rev-parse", f"{manifest['head']}^{{tree}}")}
    cwd = reconstruct("bank", manifest["head"], empty, commit)
    result = subprocess.run(
        ["git", "config", "--get", "core.hooksPath"], cwd=cwd, capture_output=True, text=True, check=False
    )
    require(result.returncode == 1, "custom hooksPath refuses bank")
    for key in ("GIT_AUTHOR_IDENT", "GIT_COMMITTER_IDENT"):
        require(
            git(cwd, "var", key).startswith(IDENTITY + " "), "approved ordinary Git identity must be provisioned first"
        )
    execute("identity-origin", ["git", "config", "--show-origin", "--get-regexp", r"user\."], cwd)
    execute(
        "install-hooks", ["poly", "hooks", "install", "--hook-type", "pre-commit", "--hook-type", "commit-msg"], cwd
    )
    hooks = Path(git(cwd, "rev-parse", "--git-path", "hooks"))
    if not hooks.is_absolute():
        hooks = cwd / hooks
    for name, digest in HOOK_DIGESTS.items():
        require(sha256((hooks / name).read_bytes()) == digest, f"normal {name} shim differs from audited hook")
    branch = "takeover-result"
    execute("bank-branch", ["git", "switch", "-c", branch], cwd)
    merge = execute(
        "preserve-integration-merge", ["git", "merge", "--no-commit", "--no-ff", INTEGRATION], cwd, check=False
    )
    require(merge.returncode in {0, 1}, "integration merge failed outside conflicts")
    require(git(cwd, "rev-parse", "--verify", "MERGE_HEAD") == INTEGRATION, "a genuine integration merge is required")
    unmerged = set(git(cwd, "diff", "--name-only", "--diff-filter=U").splitlines())
    allowed = set(manifest["allowed_paths"])
    require(unmerged <= allowed, "integration has an unreviewed conflict")
    for relative in sorted(allowed):
        proven = candidate / relative
        target = cwd / relative
        require(not target.is_symlink(), "bank path is a symlink")
        if proven.is_file():
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(proven.read_bytes())
            mode = git(candidate, "ls-files", "--stage", "--", relative).split()[0]
            target.chmod(0o755 if mode == "100755" else 0o644)
            git(cwd, "add", "--", relative)
        else:
            require(not target.exists(), "an unexpected deletion refuses banking")
    require(not git(cwd, "diff", "--name-only", "--diff-filter=U"), "conflicts remain")
    require(
        git(cwd, "write-tree") == manifest["candidate"]["expected_tree"] and not git(cwd, "diff", "--name-only"),
        "merged candidate is not the proven tree",
    )
    execute("uncached-pre-commit", ["poly", "hooks", "run", "--no-cache", "pre-commit"], cwd)
    require(
        git(cwd, "write-tree") == manifest["candidate"]["expected_tree"] and not git(cwd, "diff", "--name-only"),
        "hooks changed the proven tree",
    )
    message = EVIDENCE / "bank-message.txt"
    message.write_text(manifest["bank_message"].rstrip() + "\n")
    execute("normal-commit", ["git", "commit", "-F", str(message)], cwd)
    require(
        git(cwd, "rev-parse", "HEAD^{tree}") == manifest["candidate"]["expected_tree"],
        "committed tree is not proven candidate",
    )
    parents = git(cwd, "show", "-s", "--format=%P", "HEAD").split()
    require(parents == [manifest["head"], INTEGRATION], "integration/head parents were not preserved")
    execute("bank-original-ancestry", ["git", "merge-base", "--is-ancestor", ORIGINAL_HEAD, "HEAD"], cwd)
    execute("bank-integration-ancestry", ["git", "merge-base", "--is-ancestor", INTEGRATION, "HEAD"], cwd)
    identity = git(cwd, "show", "-s", "--format=%an <%ae>|%cn <%ce>", "HEAD")
    require(identity == IDENTITY + "|" + IDENTITY, "commit identity mismatch")
    bundle = EVIDENCE / "additive.bundle"
    execute("bundle-create", ["git", "bundle", "create", str(bundle), f"{manifest['head']}..{branch}"], cwd)
    execute("bundle-verify", ["git", "bundle", "verify", str(bundle)], cwd)
    receipt = {
        "run_id": os.environ["GITHUB_RUN_ID"],
        "run_attempt": os.environ["GITHUB_RUN_ATTEMPT"],
        "workflow_sha": os.environ["GITHUB_SHA"],
        "patch_commit": commit,
        "bundle_sha256": sha256(bundle.read_bytes()),
        "head": git(cwd, "rev-parse", "HEAD"),
        "parents": parents,
        "tree": git(cwd, "rev-parse", "HEAD^{tree}"),
        "identity": identity,
    }
    (EVIDENCE / "bank-receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")


def main() -> None:
    EVIDENCE.mkdir(exist_ok=True)
    require(
        os.environ.get("GITHUB_ACTIONS") == "true", "this script only runs on Actions; laptop execution is forbidden"
    )
    require(
        os.environ.get("GITHUB_REPOSITORY") == REPOSITORY and os.environ.get("GITHUB_ACTOR") == "tobocop2",
        "wrong repository/actor",
    )
    require(os.environ.get("GITHUB_REF", "").startswith("refs/heads/ci/pr568-actions-proof"), "wrong proof ref")
    phase = os.environ.get("TAKEOVER_PHASE", "")
    require(phase in {"candidate", "controls", "bank"}, "unknown fixed proof phase")
    execute("rust-version", ["rustc", "-Vv"], ROOT)
    execute("poly-version", ["poly", "--version"], ROOT)
    execute("task-version", ["task", "--version"], ROOT)
    execute("runner", ["uname", "-a"], ROOT)
    manifest, commit = fetch_manifest()
    if phase == "candidate":
        candidate_gate(manifest, commit)
    elif phase == "controls":
        control_arms(manifest, commit)
    else:
        bank(manifest, commit)
    (EVIDENCE / "done.json").write_text(
        json.dumps({"phase": phase, "status": "success", "workflow_sha": os.environ["GITHUB_SHA"]}) + "\n"
    )
    print(f"PR568_DONE phase={phase} rc=0", flush=True)


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, subprocess.SubprocessError, KeyError) as error:
        EVIDENCE.mkdir(exist_ok=True)
        (EVIDENCE / "failure.txt").write_text(str(error) + "\n")
        print(f"PR568_UNRESOLVED {error}", file=sys.stderr)
        sys.exit(1)
