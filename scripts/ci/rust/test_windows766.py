import hashlib
import json
from collections.abc import Callable
from pathlib import Path
from unittest.mock import patch

from windows766 import compilation_environment, log_path, parse_counts, parse_listing, validate_swift_checksum_advance


def rejects(call: Callable[[], object]) -> None:
    try:
        call()
    except ValueError:
        return
    raise AssertionError("invalid diagnostic evidence was accepted")


def test_accepts_one_real_execution() -> None:
    result = parse_counts(
        "running 1 test\ntest x ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 14 filtered out"
    )
    assert result.executed == 1
    assert result.failed == 0


def test_accepts_one_failed_execution_as_evidence() -> None:
    result = parse_counts(
        "running 1 test\ntest x ... FAILED\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 14 filtered out"
    )
    assert result.executed == 1
    assert result.failed == 1


def test_rejects_zero_work_ignored_and_contradictory_counts() -> None:
    for passed, failed, ignored in [(0, 0, 0), (0, 0, 1), (1, 0, 1), (2, 0, 0)]:
        text = f"running 1 test\ntest result: ok. {passed} passed; {failed} failed; {ignored} ignored; 0 measured; 14 filtered out"
        rejects(lambda: parse_counts(text))


def test_rejects_chrome_skip_and_missing_or_duplicate_summaries() -> None:
    valid = "running 1 test\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 14 filtered out"
    for text in [
        "",
        valid + "\nskipping x because no usable Chrome was found",
        valid + "\n" + valid,
        valid.replace("running 1 test", "running 0 tests"),
        valid.replace("ok. 1 passed; 0 failed", "FAILED. 1 passed; 0 failed"),
    ]:
        rejects(lambda: parse_counts(text))


def test_rejects_missing_extra_and_wrong_collected_tests() -> None:
    assert parse_listing("expected: test\n", "expected") == 1
    for text in ["", "other: test", "expected: test\nother: test"]:
        rejects(lambda: parse_listing(text, "expected"))


def test_keeps_listing_logs_separate_from_execution_logs() -> None:
    assert log_path(Path("x.list"), ".stdout.log") != log_path(Path("x"), ".stdout.log")


def test_requires_direct_compilation_and_records_the_test_profile() -> None:
    clean = {"RUSTC_WRAPPER": "", "RUSTC_WORKSPACE_WRAPPER": ""}
    with patch.dict("os.environ", clean):
        assert compilation_environment()["cargo_profile"] == "test"
    for name in clean:
        with patch.dict("os.environ", {**clean, name: "sccache"}):
            rejects(compilation_environment)


def swift_fixture() -> bytes:
    return b'let name = "Crawlberg"\nchecksum: "__ALEF_SWIFT_CHECKSUM__"\nchecksum: "__ALEF_SWIFT_CHECKSUM__"\n'


def test_accepts_only_uniform_swift_checksum_metadata_advance() -> None:
    original = swift_fixture()
    checksum = b"ab" * 32
    tagged = original.replace(b"__ALEF_SWIFT_CHECKSUM__", checksum)
    proof = validate_swift_checksum_advance(original, tagged, ["Package.swift"], 1)
    assert proof.checksum == checksum.decode("ascii")
    assert proof.placeholder_count == 2
    assert proof.original_package_sha256 == hashlib.sha256(original).hexdigest()
    assert proof.tagged_package_sha256 == hashlib.sha256(tagged).hexdigest()


def test_rejects_swift_metadata_changes_in_other_paths() -> None:
    original = swift_fixture()
    tagged = original.replace(b"__ALEF_SWIFT_CHECKSUM__", b"ab" * 32)
    for paths in [[], ["Cargo.lock"], ["Package.swift", "Cargo.lock"], ["crates/crawlberg/src/browser_pool.rs"]]:
        rejects(lambda: validate_swift_checksum_advance(original, tagged, paths, 1))


def test_rejects_other_swift_bytes_and_multiple_metadata_commits() -> None:
    original = swift_fixture()
    tagged = original.replace(b"__ALEF_SWIFT_CHECKSUM__", b"ab" * 32)
    for changed in [tagged + b"\n", tagged.replace(b"Crawlberg", b"Different")]:
        rejects(lambda: validate_swift_checksum_advance(original, changed, ["Package.swift"], 1))
    for count in [0, 2]:
        rejects(lambda: validate_swift_checksum_advance(original, tagged, ["Package.swift"], count))


def test_rejects_swift_advance_without_an_original_placeholder() -> None:
    original = swift_fixture().replace(b"__ALEF_SWIFT_CHECKSUM__", b"ab" * 32)
    rejects(lambda: validate_swift_checksum_advance(original, original, ["Package.swift"], 1))


def test_rejects_different_nonlowercase_or_wrong_length_checksums() -> None:
    original = swift_fixture()
    placeholder = b"__ALEF_SWIFT_CHECKSUM__"
    different = original.replace(placeholder, b"ab" * 32, 1).replace(placeholder, b"cd" * 32)
    for tagged in [different, original.replace(placeholder, b"AB" * 32), original.replace(placeholder, b"a" * 63)]:
        rejects(lambda: validate_swift_checksum_advance(original, tagged, ["Package.swift"], 1))


if __name__ == "__main__":
    if not __debug__:
        raise RuntimeError("harness controls require Python assertions enabled")
    cases = (
        test_accepts_one_real_execution,
        test_accepts_one_failed_execution_as_evidence,
        test_rejects_zero_work_ignored_and_contradictory_counts,
        test_rejects_chrome_skip_and_missing_or_duplicate_summaries,
        test_rejects_missing_extra_and_wrong_collected_tests,
        test_keeps_listing_logs_separate_from_execution_logs,
        test_requires_direct_compilation_and_records_the_test_profile,
        test_accepts_only_uniform_swift_checksum_metadata_advance,
        test_rejects_swift_metadata_changes_in_other_paths,
        test_rejects_other_swift_bytes_and_multiple_metadata_commits,
        test_rejects_swift_advance_without_an_original_placeholder,
        test_rejects_different_nonlowercase_or_wrong_length_checksums,
    )
    observed = 0
    for case in cases:
        case()
        observed += 1
    assert observed == 12
    print(json.dumps({"expected_guardrail_cases": 12, "observed_guardrail_cases": observed}))
