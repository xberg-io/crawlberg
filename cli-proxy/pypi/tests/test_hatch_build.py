"""Tests for the wheel build hook that repackages the released CLI archives."""

from __future__ import annotations

import io
import tarfile
import zipfile
from pathlib import Path

import pytest
from hatch_build import CustomBuildHook

_GNU_TARGET = "x86_64-unknown-linux-gnu"
_MUSL_TARGET = "aarch64-unknown-linux-musl"
_WINDOWS_TARGET = "x86_64-pc-windows-msvc"


def _hook(root: Path) -> CustomBuildHook:
    return CustomBuildHook(
        str(root),
        config={},
        build_config={},
        metadata=None,
        directory=str(root / "dist"),
        target_name="wheel",
    )


def _write_tar(root: Path, target: str, members: dict[str, bytes]) -> None:
    dist = root / "dist"
    dist.mkdir(parents=True, exist_ok=True)
    with tarfile.open(dist / f"crawlberg-cli-{target}.tar.gz", "w:gz") as archive:
        for name, payload in members.items():
            info = tarfile.TarInfo(name)
            info.size = len(payload)
            archive.addfile(info, io.BytesIO(payload))


def _build(root: Path, target: str, monkeypatch) -> dict:
    monkeypatch.setenv("CRAWLBERG_CLI_TARGET", target)
    build_data: dict = {}
    _hook(root).initialize("1.0.0", build_data)
    return build_data


def test_should_do_nothing_without_a_target(tmp_path: Path, monkeypatch) -> None:
    monkeypatch.delenv("CRAWLBERG_CLI_TARGET", raising=False)
    build_data: dict = {}
    _hook(tmp_path).initialize("1.0.0", build_data)
    assert build_data == {}


def test_should_bundle_the_binary_and_tag_the_wheel_for_the_platform(tmp_path: Path, monkeypatch) -> None:
    _write_tar(tmp_path, _GNU_TARGET, {f"crawlberg-cli-{_GNU_TARGET}/crawlberg": b"\x7fELF"})

    build_data = _build(tmp_path, _GNU_TARGET, monkeypatch)

    assert set(build_data["force_include"].values()) == {f"crawlberg_cli/bin/{_GNU_TARGET}/crawlberg"}
    assert build_data["tag"] == "py3-none-manylinux_2_28_x86_64"
    assert build_data["pure_python"] is False


def test_should_preserve_the_relative_paths_of_the_staged_payload(tmp_path: Path, monkeypatch) -> None:
    stage = f"crawlberg-cli-{_MUSL_TARGET}"
    _write_tar(
        tmp_path,
        _MUSL_TARGET,
        {f"{stage}/crawlberg": b"\x7fELF", f"{stage}/lib/libextra.so": b"lib", f"{stage}/LICENSE": b"mit"},
    )

    build_data = _build(tmp_path, _MUSL_TARGET, monkeypatch)

    assert set(build_data["force_include"].values()) == {
        f"crawlberg_cli/bin/{_MUSL_TARGET}/crawlberg",
        f"crawlberg_cli/bin/{_MUSL_TARGET}/lib/libextra.so",
        f"crawlberg_cli/bin/{_MUSL_TARGET}/LICENSE",
    }


def test_should_bundle_the_windows_exe_from_a_zip(tmp_path: Path, monkeypatch) -> None:
    dist = tmp_path / "dist"
    dist.mkdir()
    with zipfile.ZipFile(dist / f"crawlberg-cli-{_WINDOWS_TARGET}.zip", "w") as archive:
        archive.writestr(f"crawlberg-cli-{_WINDOWS_TARGET}/crawlberg.exe", b"MZ")

    build_data = _build(tmp_path, _WINDOWS_TARGET, monkeypatch)

    assert set(build_data["force_include"].values()) == {f"crawlberg_cli/bin/{_WINDOWS_TARGET}/crawlberg.exe"}
    assert build_data["tag"] == "py3-none-win_amd64"


def test_should_refuse_to_build_an_empty_platform_wheel(tmp_path: Path, monkeypatch) -> None:
    with pytest.raises(RuntimeError, match="refusing to build an empty platform wheel"):
        _build(tmp_path, _GNU_TARGET, monkeypatch)


def test_should_reject_an_archive_member_that_escapes_the_extraction_directory(tmp_path: Path, monkeypatch) -> None:
    _write_tar(tmp_path, _GNU_TARGET, {"../escape": b"x", "crawlberg": b"\x7fELF"})
    with pytest.raises(RuntimeError, match="escapes extraction directory"):
        _build(tmp_path, _GNU_TARGET, monkeypatch)
