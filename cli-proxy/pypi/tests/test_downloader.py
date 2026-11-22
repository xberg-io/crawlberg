"""Tests for the release pinning and checksum verification of the runtime downloader."""

from __future__ import annotations

import hashlib
from pathlib import Path

import pytest
from crawlberg_cli import downloader

_ARCHIVE_NAME = "crawlberg-cli-aarch64-apple-darwin.tar.gz"
_PAYLOAD = b"archive bytes"
_DIGEST = hashlib.sha256(_PAYLOAD).hexdigest()


@pytest.mark.parametrize(
    ("package_version", "tag"),
    [
        ("1.13.1", "v1.13.1"),
        ("1.14.0rc1", "v1.14.0-rc.1"),
        ("1.14.0a2", "v1.14.0-alpha.2"),
        ("1.14.0b3", "v1.14.0-beta.3"),
    ],
)
def test_should_derive_the_release_tag_from_the_package_version(package_version: str, tag: str) -> None:
    assert downloader._release_tag(package_version) == tag


def test_should_prefer_an_explicit_override_over_the_package_version() -> None:
    assert downloader._release_tag("1.13.1", "v1.12.0") == "v1.12.0"


@pytest.mark.parametrize("override", ["latest", "1.12.0", "v1.12", "../v1.0.0"])
def test_should_reject_a_malformed_override(override: str) -> None:
    with pytest.raises(RuntimeError, match="refusing to resolve release tag"):
        downloader._release_tag("1.13.1", override)


def test_should_reject_a_version_it_cannot_map_to_a_tag() -> None:
    with pytest.raises(RuntimeError, match="cannot derive a release tag"):
        downloader._release_tag("1.13.1.dev4")


def test_should_resolve_exactly_the_release_of_the_installed_version(monkeypatch) -> None:
    requested: list[str] = []

    def fake_get(url: str, accept: str | None = None) -> bytes:
        requested.append(url)
        return b'{"tag_name": "v1.13.1", "assets": [{"name": "crawlberg-cli-aarch64-apple-darwin.tar.gz"}]}'

    monkeypatch.setattr(downloader, "_http_get", fake_get)
    monkeypatch.setattr(downloader, "_package_version", lambda: "1.13.1")
    monkeypatch.setattr(downloader, "_target_triple", lambda: "aarch64-apple-darwin")
    monkeypatch.delenv(downloader.VERSION_ENV, raising=False)

    tag, archive, _ = downloader._resolve_release()

    assert requested == ["https://api.github.com/repos/xberg-io/crawlberg/releases/tags/v1.13.1"]
    assert tag == "v1.13.1"
    assert archive["name"] == _ARCHIVE_NAME


def test_should_accept_a_matching_checksum(tmp_path: Path) -> None:
    archive = tmp_path / _ARCHIVE_NAME
    archive.write_bytes(_PAYLOAD)
    downloader._verify_checksum(archive, _ARCHIVE_NAME, f"{_DIGEST}  {_ARCHIVE_NAME}\n")


def test_should_reject_a_tampered_archive(tmp_path: Path) -> None:
    archive = tmp_path / _ARCHIVE_NAME
    archive.write_bytes(b"tampered")
    with pytest.raises(RuntimeError, match="checksum mismatch"):
        downloader._verify_checksum(archive, _ARCHIVE_NAME, f"{_DIGEST}  {_ARCHIVE_NAME}\n")


def test_should_refuse_an_archive_with_no_checksum_entry(tmp_path: Path) -> None:
    archive = tmp_path / _ARCHIVE_NAME
    archive.write_bytes(_PAYLOAD)
    with pytest.raises(RuntimeError, match="no checksum entry"):
        downloader._verify_checksum(archive, _ARCHIVE_NAME, f"{_DIGEST}  other.tar.gz\n")
